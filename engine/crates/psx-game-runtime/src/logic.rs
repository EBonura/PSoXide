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
//! Visibility policy: the per-tick player-touch scan gates on the owner's
//! spatial mask (the active room set); the delay queue and re-arm timers
//! process globally so a timed chain never freezes when the player
//! looks away (hl parity: visibility gates monster thinking, not logic).
//!
//! Crate rules hold: no statics, no unsafe, `const N` capacities,
//! `&'static` cooked records, all-zero [`LogicRuntime::EMPTY`].

use psx_level::{
    door_flags, logic_flags, logic_kind, persistent_flag_slot, switch_flags, LevelLogicRecord,
    StableId, LOGIC_NAME_NONE,
};

use crate::save::SaveBlock;

/// What a use asks of the record it reaches. Records fired by an output
/// (relays, delayed chains, interact prompts) always toggle; the explicit
/// on/off forms let a script force a door or gate input to a known state.
///
/// The discriminants are this runtime's own two-bit packing in a queued
/// event; nothing cooked stores them.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseCode {
    /// Flip the record's current state.
    Toggle = 0,
    /// Force the record on, open or satisfied.
    On = 1,
    /// Force the record off, closed or unsatisfied.
    Off = 2,
    /// Clear a locked door's lock without moving it. Only doors react.
    Unlock = 3,
}

impl UseCode {
    const fn from_bits(bits: u16) -> Self {
        match bits & 0x3 {
            1 => Self::On,
            2 => Self::Off,
            3 => Self::Unlock,
            _ => Self::Toggle,
        }
    }
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

    const fn use_code(self) -> UseCode {
        UseCode::from_bits(self.meta >> 1)
    }

    const fn meta_for(active: bool, use_code: UseCode) -> u16 {
        (active as u16) | ((use_code as u16) << 1)
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

/// Where a sliding door is, from [`LogicRuntime::door_slide`].
// Read by the game, never built outside this module.
#[allow(clippy::exhaustive_structs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorSlide {
    /// Offset from the closed position, engine units.
    pub offset: [i32; 3],
    /// Fully open: nothing of the door is left in the doorway.
    pub cleared: bool,
}

/// True when a door record slides (otherwise it opens instantly).
fn door_slides(record: &LevelLogicRecord) -> bool {
    record.spawnflags & door_flags::DIR_MASK != door_flags::DIR_NONE
        && record.arg0 > 0
        && record.arg1 > 0
}

/// True when `player` stands on the side `record` refuses.
fn refuses_side(record: &LevelLogicRecord, player: [i32; 3]) -> bool {
    match (record.spawnflags & door_flags::REFUSE_MASK) >> door_flags::REFUSE_SHIFT {
        door_flags::REFUSE_EAST => player[0] > record.x,
        door_flags::REFUSE_WEST => player[0] < record.x,
        door_flags::REFUSE_SOUTH => player[2] > record.z,
        door_flags::REFUSE_NORTH => player[2] < record.z,
        _ => false,
    }
}

/// Per-tick inputs: the player pose (trigger-volume touches are
/// player-only this slice). Which records scan is the owner's spatial mask,
/// see [`LogicRuntime::set_spatial_active_mask`].
#[derive(Clone, Copy)]
pub struct LogicTickInput {
    /// Player position, engine units.
    pub player: [i32; 3],
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
    /// Slide progress of a door with motion: 0 = closed, the door's
    /// `arg1` ticks = fully open.
    progress: [u16; MAX_LOGIC],
    /// 1 while a door refuses to open until unlocked.
    locked: [u8; MAX_LOGIC],
    /// Fired marks the owning game drains for terminal effects (one
    /// bit per record).
    fired_marks: [u32; LOGIC_FIRED_WORDS],
    /// Refusal marks (a locked or wrong-side door was asked to open) the
    /// owning game drains to show the door's message.
    refused_marks: [u32; LOGIC_FIRED_WORDS],
    /// Player position at the last tick, for the one-way side test.
    player: [i32; 3],
    /// The delay queue.
    events: [LogicEvent; MAX_EVENTS],
    /// Rolling stats.
    queue_drops: u16,
    depth_drops: u16,
    fired: u16,
    /// Owner-supplied per-record activation for touch scans.
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
        progress: [0; MAX_LOGIC],
        locked: [0; MAX_LOGIC],
        fired_marks: [0; LOGIC_FIRED_WORDS],
        refused_marks: [0; LOGIC_FIRED_WORDS],
        player: [0; 3],
        events: [LogicEvent::INACTIVE; MAX_EVENTS],
        queue_drops: 0,
        depth_drops: 0,
        fired: 0,
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
            if record.kind == logic_kind::DOOR {
                self.locked[index] = u8::from(record.spawnflags & door_flags::LOCKED != 0);
                // A sliding door that starts open starts fully slid.
                if door_slides(record) && self.counter[index] != 0 {
                    self.progress[index] = record.arg1;
                }
            }
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

    /// Select which records the owner considers spatially active (bit `i`
    /// is record `i`). Trigger volumes only scan while their bit is set.
    // psx-numeric-allow-next-line: mirrors the fixed 64-record activation mask; bit ops only
    pub fn set_spatial_active_mask(&mut self, mask: u64) {
        self.spatial_active_mask = mask;
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

    /// The save flag record `index` owns, or `None` when it is not
    /// persistent (or `ids` has no entry for it). `ids` is the cooked
    /// stable-id table parallel to `records`.
    #[must_use]
    pub fn persistent_slot(
        records: &'static [LevelLogicRecord],
        ids: &[StableId],
        index: usize,
    ) -> Option<usize> {
        let record = records.get(index)?;
        if record.flags & logic_flags::PERSISTENT == 0 {
            return None;
        }
        Some(usize::from(persistent_flag_slot(*ids.get(index)?)))
    }

    /// Write record `index`'s state into `save` when it is persistent: the
    /// flag reads "differs from the authored start" (a door that starts open
    /// and was closed is set), so an untouched record leaves its bit clear.
    /// Returns whether `save` changed, so the owner can mark it dirty.
    pub fn store_persistent(
        &self,
        records: &'static [LevelLogicRecord],
        ids: &[StableId],
        index: usize,
        save: &mut SaveBlock,
    ) -> bool {
        let Some(slot) = Self::persistent_slot(records, ids, index) else {
            return false;
        };
        if index >= self.count() || slot >= usize::from(save.flag_count) {
            return false;
        }
        let start_on = records[index].flags & logic_flags::START_ON != 0;
        let differs = (self.counter[index] != 0) != start_on;
        if save.flag(slot) == differs {
            return false;
        }
        if differs {
            save.set_flag(slot);
        } else {
            save.clear_flag(slot);
        }
        true
    }

    /// Apply the persistent state in `save` to a freshly initialised runtime
    /// (after [`Self::init_from_records`], at boot or on respawn). Records
    /// whose bit is clear keep their authored start; every transient record
    /// is untouched, so it resets exactly as before.
    pub fn restore_persistent(
        &mut self,
        records: &'static [LevelLogicRecord],
        ids: &[StableId],
        save: &SaveBlock,
    ) {
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            if let Some(slot) = Self::persistent_slot(records, ids, index) {
                if slot < usize::from(save.flag_count) && save.flag(slot) {
                    self.restore_changed(index, &records[index]);
                }
            }
            index += 1;
        }
    }

    /// Put persistent record `index` in its "differs from the authored
    /// start" state.
    fn restore_changed(&mut self, index: usize, record: &LevelLogicRecord) {
        let on = record.flags & logic_flags::START_ON == 0;
        self.counter[index] = i16::from(on);
        match record.kind {
            logic_kind::DOOR => {
                if door_slides(record) {
                    self.progress[index] = if on { record.arg1 } else { 0 };
                }
                // An open door is not locked; a closed one is back to its
                // authored lock (unlocking is not saved).
                if on {
                    self.locked[index] = 0;
                }
            }
            // A spent one-shot lever stays spent.
            logic_kind::SWITCH if record.wait_ticks < 0 => {
                self.state[index] = LogicState::Removed as u8;
            }
            _ => {}
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

    /// Where a door with motion is: its offset from the closed position in
    /// engine units, and whether it has fully cleared (the game hides it and
    /// frees its collision then; until then it is solid where it is).
    /// `None` for a door that opens instantly, and for any other record.
    #[must_use]
    pub fn door_slide(
        &self,
        records: &'static [LevelLogicRecord],
        index: usize,
    ) -> Option<DoorSlide> {
        let record = records.get(index)?;
        if index >= self.count() || record.kind != logic_kind::DOOR || !door_slides(record) {
            return None;
        }
        // 32767 * 65535 still fits an i32; saturating keeps a hand-built
        // record with a larger distance from overflowing.
        let travelled = i32::from(record.arg0).saturating_mul(i32::from(self.progress[index]))
            / i32::from(record.arg1);
        let (x, y, z) = match record.spawnflags & door_flags::DIR_MASK {
            door_flags::DIR_POS_X => (travelled, 0, 0),
            door_flags::DIR_NEG_X => (-travelled, 0, 0),
            door_flags::DIR_UP => (0, travelled, 0),
            door_flags::DIR_DOWN => (0, -travelled, 0),
            door_flags::DIR_POS_Z => (0, 0, travelled),
            _ => (0, 0, -travelled),
        };
        Some(DoorSlide {
            offset: [x, y, z],
            cleared: self.counter[index] != 0 && self.progress[index] >= record.arg1,
        })
    }

    /// True while door `index` refuses to open until unlocked.
    #[must_use]
    pub fn door_locked(&self, index: usize) -> bool {
        index < self.count() && self.locked[index] != 0
    }

    /// Drain the refusal mark for record `index`: true once per refusal.
    pub fn take_refused(&mut self, index: usize) -> bool {
        let word = index / 32;
        if index >= self.count() || word >= self.refused_marks.len() {
            return false;
        }
        let mask = 1u32 << (index % 32);
        let hit = self.refused_marks[word] & mask != 0;
        self.refused_marks[word] &= !mask;
        hit
    }

    /// True when any refusal mark is pending.
    #[must_use]
    pub fn any_refused(&self) -> bool {
        self.refused_marks.iter().any(|word| *word != 0)
    }

    /// On/off state of a [`logic_kind::SWITCH`] record (a lever that has
    /// been pulled reads on until it is pulled back).
    #[must_use]
    pub fn switch_on(&self, index: usize) -> bool {
        self.door_open(index)
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
        code: UseCode,
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
        code: UseCode,
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
    /// records, and run the player-touch scan over trigger volumes the
    /// owner's spatial mask marks active.
    pub fn tick(&mut self, records: &'static [LevelLogicRecord], input: LogicTickInput, now: u32) {
        if self.count == 0 {
            return;
        }
        self.player = input.player;
        self.process_events(records, now);
        self.advance_doors(records);
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
            // The owner's spatial mask gates the touch scan for scan cost.
            let activation_allows = index < 64 && self.spatial_active_mask & (1u64 << index) != 0;
            if record.kind == logic_kind::TRIGGER_VOLUME
                && activation_allows
                && point_in_aabb(input.player, record.min, record.max)
                && self.master_satisfied(records, record.master)
            {
                self.activate(records, index, UseCode::Toggle, now, 0);
            }
            index += 1;
        }
    }

    /// Move every sliding door one tick toward its commanded state.
    fn advance_doors(&mut self, records: &'static [LevelLogicRecord]) {
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            if record.kind == logic_kind::DOOR && door_slides(record) {
                let progress = self.progress[index];
                if self.counter[index] != 0 {
                    self.progress[index] = progress.saturating_add(1).min(record.arg1);
                } else {
                    self.progress[index] = progress.saturating_sub(1);
                }
            }
            index += 1;
        }
    }

    /// The first closed, live door named `target` that would refuse to open
    /// right now (locked, or the player is on its refused side).
    fn refusing_target(&self, records: &'static [LevelLogicRecord], target: u16) -> Option<usize> {
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            if record.kind == logic_kind::DOOR
                && record.targetname == target
                && self.counter[index] == 0
                && LogicState::from_raw(self.state[index]) != LogicState::Removed
                && self.door_refuses(record, index)
            {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    fn door_refuses(&self, record: &LevelLogicRecord, index: usize) -> bool {
        self.locked[index] != 0 || refuses_side(record, self.player)
    }

    fn mark_refused(&mut self, index: usize) {
        let word = index / 32;
        if word < self.refused_marks.len() {
            self.refused_marks[word] |= 1u32 << (index % 32);
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

    fn enqueue(&mut self, at: u32, target: u16, killtarget: u16, code: UseCode) {
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
        code: UseCode,
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
        code: UseCode,
        now: u32,
        depth: u8,
    ) {
        let record = &records[index];
        if code == UseCode::Unlock && record.kind != logic_kind::DOOR {
            return;
        }
        match record.kind {
            logic_kind::MULTISOURCE => {
                // AND gate: inputs push the satisfied count around;
                // becoming satisfied fires the gate's own target once.
                let required = i16::try_from(record.arg0.max(1)).unwrap_or(i16::MAX);
                let was_satisfied = self.counter[index] >= required;
                self.counter[index] = match code {
                    UseCode::On => self.counter[index].saturating_add(1).min(required),
                    UseCode::Off => self.counter[index].saturating_sub(1).max(0),
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
                    self.dispatch_outputs(records, record, now, depth, UseCode::Toggle);
                }
                return;
            }
            logic_kind::DOOR => {
                if code == UseCode::Unlock {
                    self.locked[index] = 0;
                    return;
                }
                let open = self.counter[index] != 0;
                let next = match code {
                    UseCode::On => true,
                    UseCode::Off => false,
                    _ => !open,
                };
                if next == open {
                    return;
                }
                if next && self.door_refuses(record, index) {
                    self.mark_refused(index);
                    return;
                }
                self.counter[index] = next as i16;
            }
            logic_kind::SWITCH => {
                // A lever: flip (or force) its own state, then tell the
                // target the new state outright. An explicit On/Off keeps
                // the target in step with the lever even when the target
                // was also moved by something else.
                let on = self.counter[index] != 0;
                let next = match code {
                    UseCode::On => true,
                    UseCode::Off => false,
                    _ => !on,
                };
                if next == on {
                    return;
                }
                let unlocks = record.spawnflags & switch_flags::UNLOCK != 0;
                // A door that would refuse says so, and the lever stays
                // unspent: pressing it again after unlocking must still work.
                if next && !unlocks && record.delay_ticks == 0 {
                    if let Some(door) = self.refusing_target(records, record.target) {
                        self.mark_refused(door);
                        return;
                    }
                }
                self.counter[index] = next as i16;
                self.mark_fired(index);
                if unlocks {
                    // Unlocking is one way: turning the lever back sends nothing.
                    if next {
                        self.dispatch_outputs(records, record, now, depth, UseCode::Unlock);
                    }
                } else {
                    let out = if next { UseCode::On } else { UseCode::Off };
                    self.dispatch_outputs(records, record, now, depth, out);
                }
                self.apply_wait(index, record, now);
                return;
            }
            // MESSAGE / CHECKPOINT / TRIGGER_VOLUME / RELAY: the
            // reaction is the fire mark plus output dispatch.
            _ => {}
        }
        self.mark_fired(index);
        self.dispatch_outputs(records, record, now, depth, UseCode::Toggle);
        self.apply_wait(index, record, now);
    }

    /// Fire-once and re-arm bookkeeping after a record reacted.
    fn apply_wait(&mut self, index: usize, record: &LevelLogicRecord, now: u32) {
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
        code: UseCode,
    ) {
        if record.target == LOGIC_NAME_NONE && record.killtarget == LOGIC_NAME_NONE {
            return;
        }
        if record.delay_ticks > 0 {
            self.enqueue(
                now.saturating_add(u32::from(record.delay_ticks)),
                record.target,
                record.killtarget,
                code,
            );
            return;
        }
        self.kill_targets(records, record.killtarget);
        self.fire_targets(records, record.target, code, now, depth + 1);
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
    use psx_level::RoomIndex;

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

    fn input_at(pos: [i32; 3]) -> LogicTickInput {
        LogicTickInput { player: pos }
    }

    #[test]
    fn empty_records_tick_is_inert() {
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&[]);
        logic.set_spatial_active_mask(u64::MAX);
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
        logic.set_spatial_active_mask(u64::MAX);
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
        logic.set_spatial_active_mask(u64::MAX);
        logic.tick(&ONCE, input_at([0, 0, 0]), 1);
        assert!(logic.door_open(1));
        assert!(logic.is_removed(0), "wait -1 retires after one fire");
        // Standing in it again toggles nothing.
        logic.tick(&ONCE, input_at([0, 0, 0]), 2);
        assert!(logic.door_open(1));

        // The wait=30 trigger from the chain re-arms after 30 ticks.
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TRIGGER_DOOR_CHAIN);
        logic.set_spatial_active_mask(u64::MAX);
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
        logic.set_spatial_active_mask(u64::MAX);
        // Locked: standing in the trigger does nothing.
        logic.tick(&GATED, input_at([0, 0, 0]), 1);
        assert!(!logic.door_open(2));
        // One input on: still locked.
        logic.fire_by_name(&GATED, 4, UseCode::On, 2);
        logic.tick(&GATED, input_at([0, 0, 0]), 3);
        assert!(!logic.door_open(2));
        // Second input satisfies the gate: the trigger passes.
        logic.fire_by_name(&GATED, 4, UseCode::On, 4);
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
        logic.set_spatial_active_mask(u64::MAX);
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
        logic.set_spatial_active_mask(u64::MAX);
        logic.fire_by_name(&RING, 1, UseCode::Toggle, 1);
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
        logic.set_spatial_active_mask(u64::MAX);
        logic.fire_by_name(&KILL, 1, UseCode::Toggle, 1);
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
        logic.set_spatial_active_mask(u64::MAX);
        for now in 0..5 {
            logic.fire_by_name(&DELAYED, 1, UseCode::Toggle, now);
        }
        let stats = logic.stats();
        assert_eq!(stats.queued, 4);
        assert_eq!(stats.queue_drops, 1);
    }

    // Names: 1 = lever, 2 = door.
    static LEVER_DOOR: [LevelLogicRecord; 2] = [
        LevelLogicRecord {
            targetname: 1,
            target: 2,
            wait_ticks: 30,
            ..blank(logic_kind::SWITCH)
        },
        LevelLogicRecord {
            targetname: 2,
            ..blank(logic_kind::DOOR)
        },
    ];

    #[test]
    fn toggle_switch_drives_its_target_with_explicit_on_and_off() {
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&LEVER_DOOR);
        assert!(logic.fire_index(&LEVER_DOOR, 0, UseCode::Toggle, 1));
        assert!(logic.switch_on(0) && logic.door_open(1));
        // Cooldown: a second press inside `wait_ticks` is refused.
        assert!(!logic.fire_index(&LEVER_DOOR, 0, UseCode::Toggle, 10));
        logic.tick(&LEVER_DOOR, input_at([0; 3]), 40);
        assert!(logic.fire_index(&LEVER_DOOR, 0, UseCode::Toggle, 40));
        assert!(!logic.switch_on(0) && !logic.door_open(1));
    }

    #[test]
    fn a_door_moved_by_something_else_still_follows_the_lever() {
        // Toggle semantics would flip the door the wrong way; the lever
        // sends On/Off, so the door ends up where the lever says.
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&LEVER_DOOR);
        logic.fire_by_name(&LEVER_DOOR, 2, UseCode::On, 1);
        assert!(logic.door_open(1) && !logic.switch_on(0));
        assert!(logic.fire_index(&LEVER_DOOR, 0, UseCode::Toggle, 2));
        assert!(logic.door_open(1), "lever on: door stays open");
    }

    #[test]
    fn one_shot_switch_retires_after_its_press() {
        static ONCE: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 2,
                wait_ticks: -1,
                ..blank(logic_kind::SWITCH)
            },
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&ONCE);
        assert!(logic.fire_index(&ONCE, 0, UseCode::Toggle, 1));
        assert!(logic.door_open(1) && logic.is_removed(0));
        assert!(!logic.fire_index(&ONCE, 0, UseCode::Toggle, 2));
        assert!(logic.door_open(1));
    }

    #[test]
    fn switch_output_keeps_its_delay() {
        static DELAYED: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 2,
                delay_ticks: 10,
                ..blank(logic_kind::SWITCH)
            },
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&DELAYED);
        assert!(logic.fire_index(&DELAYED, 0, UseCode::Toggle, 5));
        assert!(!logic.door_open(1));
        logic.tick(&DELAYED, input_at([0; 3]), 15);
        assert!(logic.door_open(1), "the queued On arrives, not a Toggle");
    }

    /// A door that slides up 80 units over 4 ticks.
    const fn sliding_door(spawnflags: u16) -> LevelLogicRecord {
        LevelLogicRecord {
            targetname: 2,
            spawnflags: door_flags::DIR_UP | spawnflags,
            arg0: 80,
            arg1: 4,
            x: 100,
            z: 50,
            ..blank(logic_kind::DOOR)
        }
    }

    #[test]
    fn sliding_door_is_solid_until_it_has_cleared_and_reverses_when_closed() {
        static DOOR: [LevelLogicRecord; 1] = [sliding_door(0)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&DOOR);
        let slide = logic.door_slide(&DOOR, 0).unwrap();
        assert_eq!((slide.offset, slide.cleared), ([0, 0, 0], false));

        logic.fire_by_name(&DOOR, 2, UseCode::On, 1);
        let mut seen = [(0, false); 6];
        for (step, now) in (2..8).enumerate() {
            logic.tick(&DOOR, input_at([0; 3]), now);
            let slide = logic.door_slide(&DOOR, 0).unwrap();
            seen[step] = (slide.offset[1], slide.cleared);
        }
        // Four ticks to travel, then it stays put; only the last step is
        // cleared (hidden + passable), every earlier one is solid.
        assert_eq!(
            seen,
            [
                (20, false),
                (40, false),
                (60, false),
                (80, true),
                (80, true),
                (80, true)
            ]
        );

        logic.fire_by_name(&DOOR, 2, UseCode::Off, 9);
        logic.tick(&DOOR, input_at([0; 3]), 10);
        let slide = logic.door_slide(&DOOR, 0).unwrap();
        assert_eq!((slide.offset[1], slide.cleared), (60, false));
        for now in 11..16 {
            logic.tick(&DOOR, input_at([0; 3]), now);
        }
        assert_eq!(logic.door_slide(&DOOR, 0).unwrap().offset, [0, 0, 0]);
    }

    #[test]
    fn a_door_without_motion_has_no_slide_and_a_start_open_slider_starts_cleared() {
        static INSTANT: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 2,
            ..blank(logic_kind::DOOR)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&INSTANT);
        assert_eq!(logic.door_slide(&INSTANT, 0), None);

        static OPEN: [LevelLogicRecord; 1] = [LevelLogicRecord {
            flags: logic_flags::ENABLED | logic_flags::START_ON,
            ..sliding_door(0)
        }];
        logic.init_from_records(&OPEN);
        let slide = logic.door_slide(&OPEN, 0).unwrap();
        assert_eq!((slide.offset, slide.cleared), ([0, 80, 0], true));
    }

    #[test]
    fn a_locked_door_refuses_until_an_unlock_switch_is_pulled() {
        // 1 = unlock lever, 2 = door, 3 = open lever.
        static LOCKED: [LevelLogicRecord; 3] = [
            LevelLogicRecord {
                targetname: 1,
                target: 2,
                spawnflags: switch_flags::UNLOCK,
                wait_ticks: -1,
                ..blank(logic_kind::SWITCH)
            },
            sliding_door(door_flags::LOCKED),
            LevelLogicRecord {
                targetname: 3,
                target: 2,
                ..blank(logic_kind::SWITCH)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&LOCKED);
        assert!(logic.door_locked(1));
        // The open lever is refused and is NOT spent: its state is unchanged.
        assert!(logic.fire_index(&LOCKED, 2, UseCode::Toggle, 1));
        assert!(logic.take_refused(1) && !logic.take_refused(1));
        assert!(!logic.switch_on(2) && !logic.door_open(1));
        // Unlocking opens nothing by itself...
        assert!(logic.fire_index(&LOCKED, 0, UseCode::Toggle, 2));
        assert!(!logic.door_locked(1) && !logic.door_open(1));
        assert!(!logic.any_refused());
        // ...and the same lever now works.
        assert!(logic.fire_index(&LOCKED, 2, UseCode::Toggle, 3));
        assert!(logic.door_open(1) && !logic.any_refused());
    }

    #[test]
    fn a_one_way_door_refuses_by_the_side_the_player_stands_on() {
        // Refuses a player WEST of x = 100; the lever is one-shot.
        static ONE_WAY: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 2,
                wait_ticks: -1,
                ..blank(logic_kind::SWITCH)
            },
            sliding_door(door_flags::REFUSE_WEST << door_flags::REFUSE_SHIFT),
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&ONE_WAY);
        logic.tick(&ONE_WAY, input_at([40, 0, 50]), 1);
        assert!(logic.fire_index(&ONE_WAY, 0, UseCode::Toggle, 2));
        assert!(logic.take_refused(1), "west side: refused");
        assert!(!logic.door_open(1));
        assert!(
            !logic.is_removed(0),
            "a refused one-shot lever is not spent"
        );

        logic.tick(&ONE_WAY, input_at([160, 0, 50]), 3);
        assert!(logic.fire_index(&ONE_WAY, 0, UseCode::Toggle, 4));
        assert!(!logic.any_refused());
        assert!(
            logic.door_open(1) && logic.is_removed(0),
            "east side: opens"
        );

        // Closing is never refused, from either side.
        logic.tick(&ONE_WAY, input_at([40, 0, 50]), 5);
        logic.fire_by_name(&ONE_WAY, 2, UseCode::Off, 6);
        assert!(!logic.door_open(1) && !logic.any_refused());
    }

    #[test]
    fn a_one_way_door_on_the_z_axis_and_on_the_plane() {
        static NORTH: [LevelLogicRecord; 1] = [sliding_door(
            door_flags::REFUSE_NORTH << door_flags::REFUSE_SHIFT,
        )];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&NORTH);
        logic.tick(&NORTH, input_at([0, 0, 10]), 1);
        logic.fire_by_name(&NORTH, 2, UseCode::On, 2);
        assert!(logic.take_refused(0), "north of z = 50 is refused");
        // Standing exactly on the plane is allowed.
        logic.tick(&NORTH, input_at([0, 0, 50]), 3);
        logic.fire_by_name(&NORTH, 2, UseCode::On, 4);
        assert!(logic.door_open(0));
    }

    const PERSIST: u16 = logic_flags::ENABLED | logic_flags::PERSISTENT;
    const IDS: [StableId; 4] = [StableId(5), StableId(1_000_003), StableId(77), StableId(42)];

    // 0 = lever (persistent, one-shot), 1 = door it opens (persistent,
    // slides), 2 = a transient door, 3 = a persistent door that starts open.
    static WORLD: [LevelLogicRecord; 4] = [
        LevelLogicRecord {
            targetname: 1,
            target: 2,
            wait_ticks: -1,
            flags: PERSIST,
            ..blank(logic_kind::SWITCH)
        },
        LevelLogicRecord {
            flags: PERSIST,
            ..sliding_door(0)
        },
        LevelLogicRecord {
            targetname: 3,
            ..blank(logic_kind::DOOR)
        },
        LevelLogicRecord {
            targetname: 4,
            flags: PERSIST | logic_flags::START_ON,
            ..blank(logic_kind::DOOR)
        },
    ];

    #[test]
    fn a_persistent_door_and_lever_survive_a_death_and_transient_ones_reset() {
        let mut save = SaveBlock::new(100, 832);
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&WORLD);
        // Pull the lever, open the transient door, close the start-open door.
        assert!(logic.fire_index(&WORLD, 0, UseCode::Toggle, 1));
        logic.fire_by_name(&WORLD, 3, UseCode::On, 1);
        logic.fire_by_name(&WORLD, 4, UseCode::Off, 1);
        for index in 0..4 {
            logic.store_persistent(&WORLD, &IDS, index, &mut save);
        }
        assert!(logic.door_open(1) && logic.door_open(2) && !logic.door_open(3));

        // Death: the world re-arms from the cooked tables, then the save
        // is applied.
        logic.init_from_records(&WORLD);
        assert!(!logic.door_open(1), "re-armed before the restore");
        logic.restore_persistent(&WORLD, &IDS, &save);
        assert!(logic.door_open(1), "the persistent door is still open");
        assert!(logic.door_slide(&WORLD, 1).unwrap().cleared, "and cleared");
        assert!(logic.is_removed(0), "a spent one-shot lever stays spent");
        assert!(!logic.fire_index(&WORLD, 0, UseCode::Toggle, 9));
        assert!(!logic.door_open(2), "the transient door reset");
        assert!(
            !logic.door_open(3),
            "a start-open door that was closed stays closed"
        );
    }

    #[test]
    fn storing_reports_a_change_once_and_clears_when_toggled_back() {
        let mut save = SaveBlock::new(100, 832);
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&WORLD);
        assert!(
            !logic.store_persistent(&WORLD, &IDS, 1, &mut save),
            "untouched"
        );
        logic.fire_by_name(&WORLD, 2, UseCode::On, 1);
        assert!(logic.store_persistent(&WORLD, &IDS, 1, &mut save));
        assert!(
            !logic.store_persistent(&WORLD, &IDS, 1, &mut save),
            "no change"
        );
        logic.fire_by_name(&WORLD, 2, UseCode::Off, 2);
        assert!(logic.store_persistent(&WORLD, &IDS, 1, &mut save));
        assert!((0..832).all(|slot| !save.flag(slot)), "back to clean");
        // Non-persistent records never touch the save.
        logic.fire_by_name(&WORLD, 3, UseCode::On, 3);
        assert!(!logic.store_persistent(&WORLD, &IDS, 2, &mut save));
    }

    #[test]
    fn the_flag_follows_the_stable_id_not_the_table_position() {
        // The same door cooked into a different table position (other
        // records added before it) keeps its save bit.
        static BEFORE: [LevelLogicRecord; 1] = [LevelLogicRecord {
            flags: PERSIST,
            ..blank(logic_kind::DOOR)
        }];
        static AFTER: [LevelLogicRecord; 3] = [
            blank(logic_kind::RELAY),
            blank(logic_kind::RELAY),
            LevelLogicRecord {
                flags: PERSIST,
                ..blank(logic_kind::DOOR)
            },
        ];
        let door = StableId(123_457);
        let mut save = SaveBlock::new(100, 832);
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&BEFORE);
        logic.fire_index(&BEFORE, 0, UseCode::Toggle, 1);
        assert!(logic.store_persistent(&BEFORE, &[door], 0, &mut save));
        let mut frame = [0u8; crate::save::SAVE_BLOCK_BYTES];
        save.encode(&mut frame);
        let loaded = SaveBlock::decode(&frame, 832).unwrap();

        let mut recooked = TestLogic::EMPTY;
        recooked.init_from_records(&AFTER);
        recooked.restore_persistent(&AFTER, &[StableId(1), StableId(2), door], &loaded);
        assert!(recooked.door_open(2) && !recooked.door_open(0));
        assert_eq!(
            TestLogic::persistent_slot(&AFTER, &[StableId(1), StableId(2), door], 2),
            Some(usize::from(psx_level::persistent_flag_slot(door)))
        );
    }

    #[test]
    fn a_save_with_a_smaller_flag_table_is_ignored_not_misread() {
        let mut save = SaveBlock::new(100, 4);
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&WORLD);
        logic.fire_by_name(&WORLD, 2, UseCode::On, 1);
        // Slot of IDS[1] is 1_000_003 % 832 = 579, beyond a 4-flag table.
        assert!(!logic.store_persistent(&WORLD, &IDS, 1, &mut save));
        save.flags = [0xFF; crate::save::SAVE_FLAG_BYTES];
        logic.init_from_records(&WORLD);
        logic.restore_persistent(&WORLD, &IDS, &save);
        assert!(!logic.door_open(1));
    }

    #[test]
    fn owner_spatial_mask_gates_the_touch_scan() {
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
        logic.set_spatial_active_mask(0);
        logic.tick(&TRIGGER, input_at([0, 0, 0]), 1);
        assert!(!logic.door_open(1));

        logic.set_spatial_active_mask(1);
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
        logic.set_spatial_active_mask(u64::MAX);
        assert!(!logic.any_fired());
        assert!(logic.fire_index(&TWINS, 1, UseCode::Toggle, 1));
        assert!(!logic.door_open(0));
        assert!(logic.door_open(1));
        assert!(logic.any_fired());
        assert!(!logic.take_fired(0));
        assert!(logic.take_fired(1));
        assert!(!logic.any_fired());
        // Out of range: refused.
        assert!(!logic.fire_index(&TWINS, 2, UseCode::Toggle, 2));

        // A retired (fire-once) record refuses an indexed fire.
        static ONCE: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 5,
            wait_ticks: -1,
            ..blank(logic_kind::MESSAGE)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&ONCE);
        logic.set_spatial_active_mask(u64::MAX);
        assert!(logic.fire_index(&ONCE, 0, UseCode::Toggle, 1));
        assert!(logic.is_removed(0));
        assert!(!logic.fire_index(&ONCE, 0, UseCode::Toggle, 2));
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
        logic.set_spatial_active_mask(u64::MAX);
        assert!(!logic.fire_index(&GATED, 0, UseCode::Toggle, 1));
        logic.fire_by_name(&GATED, 4, UseCode::On, 2);
        assert!(logic.fire_index(&GATED, 0, UseCode::Toggle, 3));
    }

    #[test]
    fn trigger_touch_chains_to_a_checkpoint_record() {
        // Souls slice: a TriggerVolume targeting a CHECKPOINT record by
        // name must fire the checkpoint through the ordinary touch scan,
        // producing the fired mark the owning game's effect dispatch
        // drains (the same drain that assigns the in-memory checkpoint),
        // exactly as an interact prompt would.
        static TRIGGER_CHECKPOINT: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 7,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                wait_ticks: -1,
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 7,
                ..blank(logic_kind::CHECKPOINT)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TRIGGER_CHECKPOINT);
        logic.set_spatial_active_mask(u64::MAX);
        // Outside the volume: neither record fires.
        logic.tick(&TRIGGER_CHECKPOINT, input_at([500, 0, 0]), 1);
        assert!(!logic.any_fired());
        // Touch: the trigger fires and fans out to the checkpoint in the
        // same tick.
        logic.tick(&TRIGGER_CHECKPOINT, input_at([0, 0, 0]), 2);
        assert!(logic.take_fired(0), "trigger fired on touch");
        assert!(logic.take_fired(1), "checkpoint record fired via target");
        // The fire-once trigger retired; standing in it cannot re-fire
        // the checkpoint.
        logic.tick(&TRIGGER_CHECKPOINT, input_at([0, 0, 0]), 3);
        assert!(!logic.take_fired(1));
    }

    #[test]
    fn door_use_marks_fired_only_on_state_change() {
        // Basis for the LOGIC_DOOR_ACTIVATIONS counter: the owning game
        // counts drained DOOR fire marks, and a mark only exists when
        // the use actually changed the door state (same-state ON/OFF
        // uses return before marking).
        static DOOR: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 3,
            ..blank(logic_kind::DOOR)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&DOOR);
        logic.set_spatial_active_mask(u64::MAX);
        logic.fire_by_name(&DOOR, 3, UseCode::On, 1);
        assert!(logic.take_fired(0), "closed -> open is an activation");
        logic.fire_by_name(&DOOR, 3, UseCode::On, 2);
        assert!(!logic.take_fired(0), "already-open ON is not");
        logic.fire_by_name(&DOOR, 3, UseCode::Off, 3);
        assert!(logic.take_fired(0), "open -> closed is an activation");
        logic.fire_by_name(&DOOR, 3, UseCode::Toggle, 4);
        assert!(logic.take_fired(0), "toggle always changes state");
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
        logic.set_spatial_active_mask(u64::MAX);
        assert!(logic.is_removed(0), "flag-disabled record retires");
        assert!(logic.door_open(1), "START_ON door begins open");
    }
}
