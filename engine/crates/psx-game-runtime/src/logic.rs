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
//! Timers and hazards: a [`logic_kind::TIMER`] runs free of the player and
//! sends its targets an explicit On or Off at every change of its cycle; a
//! [`logic_kind::HAZARD`] is a volume that, while on, reports damage for the
//! player standing in it ([`LogicRuntime::take_hazard_damage`]) unless the
//! player is invulnerable. A hazard with a knockback (`arg2`, engine units)
//! also reports a push out of its volume ([`LogicRuntime::take_hazard_push`]):
//! a touch hurts a little and shoves the player out the nearer thin face,
//! whether the player walked into a live volume or it switched on around them,
//! and keeps shoving (without damage) until the hit interval allows the next. Both re-arm with the records, so a death or a
//! checkpoint rest restarts every cycle from the same beginning.
//!
//! Crate rules hold: no statics, no unsafe, `const N` capacities,
//! `&'static` cooked records, all-zero [`LogicRuntime::EMPTY`].

use psx_level::{
    door_flags, logic_flags, logic_kind, switch_flags, LevelLogicRecord, LOGIC_NAME_NONE,
    PERSISTENT_SLOT_NONE,
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

/// Slide direction (`door_flags::DIR_*`) of a door, or of a lever's handle
/// (the same bits, shifted up past the switch flags).
fn slide_dir(record: &LevelLogicRecord) -> u16 {
    match record.kind {
        logic_kind::DOOR => record.spawnflags & door_flags::DIR_MASK,
        logic_kind::SWITCH => {
            (record.spawnflags >> switch_flags::HANDLE_SHIFT) & door_flags::DIR_MASK
        }
        _ => door_flags::DIR_NONE,
    }
}

/// True when a record slides a box: a door with motion (otherwise it opens
/// instantly) or a lever with a handle.
fn door_slides(record: &LevelLogicRecord) -> bool {
    slide_dir(record) != door_flags::DIR_NONE && record.arg0 > 0 && record.arg1 > 0
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
    /// The player cannot be hurt this tick (a dash's frames, a death in
    /// progress): hazards neither hit nor spend their re-hit interval.
    pub player_invulnerable: bool,
}

/// The shove a hazard hit asks the owner to give the player: out of the
/// volume through its nearer thin-axis face, then `distance` engine units
/// along that axis (the way to the face plus the hazard's knockback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HazardPush {
    /// The axis the push runs along: `false` X, `true` Z.
    pub axis_z: bool,
    /// Push toward the larger coordinate on that axis.
    pub toward_max: bool,
    /// The player stood exactly midway: the owner may pick the face by
    /// facing or velocity and flip `toward_max`.
    pub tie: bool,
    /// Engine units to move, at least enough to leave the volume.
    pub distance: u16,
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
    /// Re-arm deadline while [`LogicState::Waiting`]. A timer keeps the tick
    /// its cycle began at here; a hazard the tick it may next hit.
    rearm_at: [u32; MAX_LOGIC],
    /// Kind-specific counter: multisource satisfied-input count,
    /// door open flag, hazard on flag, and a timer's last sent state (-1
    /// before it has sent any, then 0 off or 1 on).
    counter: [i16; MAX_LOGIC],
    /// Slide progress of a door with motion: 0 = closed, the door's
    /// `arg1` ticks = fully open. A timer uses it as "cycle start recorded".
    progress: [u16; MAX_LOGIC],
    /// 1 while a door refuses to open until unlocked. A timer uses it as
    /// "running".
    locked: [u8; MAX_LOGIC],
    /// Fired marks the owning game drains for terminal effects (one
    /// bit per record).
    fired_marks: [u32; LOGIC_FIRED_WORDS],
    /// Refusal marks (a locked or wrong-side door was asked to open) the
    /// owning game drains to show the door's message.
    refused_marks: [u32; LOGIC_FIRED_WORDS],
    /// Player position at the last tick, for the one-way side test.
    player: [i32; 3],
    /// Damage hazards dealt since [`Self::take_hazard_damage`] last ran.
    hazard_damage: u16,
    /// The push the last hazard hit asked for, until taken.
    hazard_push: Option<HazardPush>,
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
        hazard_damage: 0,
        hazard_push: None,
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
            }
            if record.kind == logic_kind::TIMER {
                // Nothing sent yet; running from load when it starts on.
                self.counter[index] = -1;
                self.locked[index] = u8::from(record.flags & logic_flags::START_ON != 0);
            }
            // A slider that starts on starts fully slid.
            if door_slides(record) && self.counter[index] != 0 {
                self.progress[index] = record.arg1;
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
    /// persistent (or `slots` has no entry for it). `slots` is the cooked
    /// slot table parallel to `records`.
    #[must_use]
    pub fn persistent_slot(
        records: &'static [LevelLogicRecord],
        slots: &[u16],
        index: usize,
    ) -> Option<usize> {
        let record = records.get(index)?;
        if record.flags & logic_flags::PERSISTENT == 0 {
            return None;
        }
        let slot = *slots.get(index)?;
        (slot != PERSISTENT_SLOT_NONE).then_some(usize::from(slot))
    }

    /// Write record `index`'s state into `save` when it is persistent: the
    /// flag reads "differs from the authored start" (a door that starts open
    /// and was closed is set), so an untouched record leaves its bit clear.
    /// Returns whether `save` changed, so the owner can mark it dirty.
    pub fn store_persistent(
        &self,
        records: &'static [LevelLogicRecord],
        slots: &[u16],
        index: usize,
        save: &mut SaveBlock,
    ) -> bool {
        let Some(slot) = Self::persistent_slot(records, slots, index) else {
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
        slots: &[u16],
        save: &SaveBlock,
    ) {
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            if let Some(slot) = Self::persistent_slot(records, slots, index) {
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
        if door_slides(record) {
            self.progress[index] = if on { record.arg1 } else { 0 };
        }
        match record.kind {
            // An open door is not locked; a closed one is back to its
            // authored lock (unlocking is not saved).
            logic_kind::DOOR if on => self.locked[index] = 0,
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
        if index >= self.count() || !door_slides(record) {
            return None;
        }
        // 32767 * 65535 still fits an i32; saturating keeps a hand-built
        // record with a larger distance from overflowing.
        let travelled = i32::from(record.arg0).saturating_mul(i32::from(self.progress[index]))
            / i32::from(record.arg1);
        let (x, y, z) = match slide_dir(record) {
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

    /// Whether record `index` is "on", the one state a visual switch follows
    /// (a plane, mesh or emitter shown while a logic record is on): a timer
    /// between its on and off ticks, a hazard that would hurt now, a door
    /// that is open, a lever thrown, a multisource that is satisfied. A
    /// record the level retired, or one that has no on state, is off.
    #[must_use]
    pub fn is_on(&self, records: &'static [LevelLogicRecord], index: usize) -> bool {
        if index >= self.count().min(records.len()) {
            return false;
        }
        let record = &records[index];
        let removed = LogicState::from_raw(self.state[index]) == LogicState::Removed;
        match record.kind {
            logic_kind::TIMER => !removed && self.counter[index] > 0,
            logic_kind::HAZARD => {
                !removed
                    && self.counter[index] != 0
                    && self.master_satisfied(records, record.master)
            }
            logic_kind::MULTISOURCE => {
                let required = i16::try_from(record.arg0.max(1)).unwrap_or(i16::MAX);
                self.counter[index] >= required
            }
            logic_kind::DOOR | logic_kind::SWITCH => self.counter[index] != 0,
            _ => false,
        }
    }

    /// Damage the hazards dealt to the player since the last call, then
    /// reset it. The owner applies it to the player's health and arms death
    /// when that kills.
    #[must_use]
    pub fn take_hazard_damage(&mut self) -> u16 {
        core::mem::take(&mut self.hazard_damage)
    }

    /// The push the last hit of a hazard with a knockback asked for, then
    /// clear it. Taken with the damage of the same tick.
    #[must_use]
    pub fn take_hazard_push(&mut self) -> Option<HazardPush> {
        self.hazard_push.take()
    }

    /// Whether the timer at `index` is running (not stopped by an Off, and
    /// started).
    #[must_use]
    pub fn timer_running(&self, index: usize) -> bool {
        index < self.count() && self.locked[index] != 0
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
            // Timers and hazards do not wait on the player's view of the
            // level: a cycle that froze when the player looked away would
            // come back out of step with the beam they are watching.
            match record.kind {
                logic_kind::TIMER => {
                    self.timer_step(records, index, now, 0);
                    index += 1;
                    continue;
                }
                logic_kind::HAZARD => {
                    self.hazard_step(records, index, input, now);
                    index += 1;
                    continue;
                }
                _ => {}
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

    /// One tick of the timer at `index`: record where its cycle began, work
    /// out whether it is in its on or off stretch, and when that differs from
    /// what it last sent, send its targets an explicit On or Off.
    fn timer_step(
        &mut self,
        records: &'static [LevelLogicRecord],
        index: usize,
        now: u32,
        depth: u8,
    ) {
        if self.locked[index] == 0 {
            return;
        }
        let record = &records[index];
        if self.progress[index] == 0 {
            // First tick of a run: the cycle begins now.
            self.progress[index] = 1;
            self.rearm_at[index] = now;
        }
        let on_ticks = u32::from(record.arg0);
        let period = on_ticks + u32::from(record.arg1);
        if period == 0 {
            return;
        }
        let elapsed = now.saturating_sub(self.rearm_at[index]);
        let phase = (elapsed % period + u32::from(record.arg2) % period) % period;
        let on = phase < on_ticks;
        if self.counter[index] == i16::from(on) {
            return;
        }
        self.counter[index] = i16::from(on);
        self.mark_fired(index);
        let code = if on { UseCode::On } else { UseCode::Off };
        self.dispatch_outputs(records, record, now, depth, code);
    }

    /// One tick of the hazard at `index`: when it is on and the player is
    /// inside and can be hurt, deal its damage and wait out its re-hit
    /// interval before the next hit.
    fn hazard_step(
        &mut self,
        records: &'static [LevelLogicRecord],
        index: usize,
        input: LogicTickInput,
        now: u32,
    ) {
        let record = &records[index];
        if self.counter[index] == 0
            || input.player_invulnerable
            || !point_in_aabb(input.player, record.min, record.max)
            || !self.master_satisfied(records, record.master)
        {
            return;
        }
        if now < self.rearm_at[index] {
            // Between hits a volume with a knockback stays solid: the player
            // is still shoved out, only the damage waits for its interval.
            if record.arg2 != 0 {
                self.hazard_push = Some(exit_push(record, input.player));
            }
            return;
        }
        self.hazard_damage = self.hazard_damage.saturating_add(record.arg0);
        self.rearm_at[index] = now.saturating_add(u32::from(record.arg1.max(1)));
        self.mark_fired(index);
        if record.arg2 != 0 {
            self.hazard_push = Some(exit_push(record, input.player));
        }
    }

    /// Move every sliding door one tick toward its commanded state.
    fn advance_doors(&mut self, records: &'static [LevelLogicRecord]) {
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            if door_slides(record) {
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
            logic_kind::TIMER => {
                // On starts the cycle from its beginning, Off stops it and
                // sends its targets Off, Toggle flips between the two.
                let running = self.locked[index] != 0;
                let next = match code {
                    UseCode::On => true,
                    UseCode::Off => false,
                    _ => !running,
                };
                if next == running {
                    return;
                }
                if next {
                    self.locked[index] = 1;
                    self.progress[index] = 0;
                    self.counter[index] = -1;
                    self.timer_step(records, index, now, depth);
                } else {
                    self.locked[index] = 0;
                    if self.counter[index] != 0 {
                        self.counter[index] = 0;
                        self.mark_fired(index);
                        self.dispatch_outputs(records, record, now, depth, UseCode::Off);
                    }
                }
                return;
            }
            logic_kind::HAZARD => {
                let on = self.counter[index] != 0;
                let next = match code {
                    UseCode::On => true,
                    UseCode::Off => false,
                    _ => !on,
                };
                self.counter[index] = i16::from(next);
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

/// The shove out of a hazard's volume: through the nearer face of its thin
/// horizontal axis (a gate is thin in the direction it is walked through),
/// then `arg2` units past it.
fn exit_push(record: &LevelLogicRecord, player: [i32; 3]) -> HazardPush {
    let width_x = record.max[0].saturating_sub(record.min[0]);
    let width_z = record.max[2].saturating_sub(record.min[2]);
    let axis_z = width_z <= width_x;
    let axis = if axis_z { 2 } else { 0 };
    let to_min = player[axis].saturating_sub(record.min[axis]).max(0);
    let to_max = record.max[axis].saturating_sub(player[axis]).max(0);
    let toward_max = to_max < to_min;
    let to_face = to_min.min(to_max);
    let distance = to_face
        .saturating_add(i32::from(record.arg2))
        .saturating_add(1)
        .clamp(1, i32::from(u16::MAX)) as u16;
    HazardPush {
        axis_z,
        toward_max,
        tie: to_min == to_max,
        distance,
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
    extern crate alloc;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
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
            arg2: 0,
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
        LogicTickInput {
            player: pos,
            player_invulnerable: false,
        }
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
    /// Cooked save slots, parallel to `WORLD`.
    const SLOTS: [u16; 4] = [5, 579, 77, 42];

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
            logic.store_persistent(&WORLD, &SLOTS, index, &mut save);
        }
        assert!(logic.door_open(1) && logic.door_open(2) && !logic.door_open(3));

        // Death: the world re-arms from the cooked tables, then the save
        // is applied.
        logic.init_from_records(&WORLD);
        assert!(!logic.door_open(1), "re-armed before the restore");
        logic.restore_persistent(&WORLD, &SLOTS, &save);
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
            !logic.store_persistent(&WORLD, &SLOTS, 1, &mut save),
            "untouched"
        );
        logic.fire_by_name(&WORLD, 2, UseCode::On, 1);
        assert!(logic.store_persistent(&WORLD, &SLOTS, 1, &mut save));
        assert!(
            !logic.store_persistent(&WORLD, &SLOTS, 1, &mut save),
            "no change"
        );
        logic.fire_by_name(&WORLD, 2, UseCode::Off, 2);
        assert!(logic.store_persistent(&WORLD, &SLOTS, 1, &mut save));
        assert!((0..832).all(|slot| !save.flag(slot)), "back to clean");
        // Non-persistent records never touch the save.
        logic.fire_by_name(&WORLD, 3, UseCode::On, 3);
        assert!(!logic.store_persistent(&WORLD, &SLOTS, 2, &mut save));
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
        let door: u16 = 300;
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
        recooked.restore_persistent(&AFTER, &[1, 2, door], &loaded);
        assert!(recooked.door_open(2) && !recooked.door_open(0));
        assert_eq!(
            TestLogic::persistent_slot(&AFTER, &[1, 2, door], 2),
            Some(usize::from(door))
        );
    }

    /// A lever with a handle slides it down while on and home while off, and a
    /// restore puts a saved lever's handle back down.
    #[test]
    fn a_lever_handle_slides_with_the_lever_and_follows_its_saved_state() {
        static HANDLE_LEVER: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 1,
            wait_ticks: -1,
            // Down, in the handle bits.
            spawnflags: door_flags::DIR_DOWN << switch_flags::HANDLE_SHIFT,
            arg0: 100,
            arg1: 10,
            flags: PERSIST,
            ..blank(logic_kind::SWITCH)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&HANDLE_LEVER);
        let at = |logic: &TestLogic| logic.door_slide(&HANDLE_LEVER, 0).expect("a handle").offset;
        assert_eq!(at(&logic), [0, 0, 0]);
        logic.fire_index(&HANDLE_LEVER, 0, UseCode::Toggle, 1);
        for now in 2..7 {
            logic.tick(&HANDLE_LEVER, input_at([0, 0, 0]), now);
        }
        assert_eq!(at(&logic), [0, -50, 0], "halfway down after five ticks");
        for now in 7..30 {
            logic.tick(&HANDLE_LEVER, input_at([0, 0, 0]), now);
        }
        assert_eq!(at(&logic), [0, -100, 0]);
        let mut save = SaveBlock::new(100, 832);
        assert!(logic.store_persistent(&HANDLE_LEVER, &[9], 0, &mut save));
        let mut after_death = TestLogic::EMPTY;
        after_death.init_from_records(&HANDLE_LEVER);
        assert_eq!(at(&after_death), [0, 0, 0]);
        after_death.restore_persistent(&HANDLE_LEVER, &[9], &save);
        assert_eq!(at(&after_death), [0, -100, 0], "the handle stays down");
    }

    #[test]
    fn a_save_with_a_smaller_flag_table_is_ignored_not_misread() {
        let mut save = SaveBlock::new(100, 4);
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&WORLD);
        logic.fire_by_name(&WORLD, 2, UseCode::On, 1);
        // Slot of SLOTS[1] is 579, beyond a 4-flag table.
        assert!(!logic.store_persistent(&WORLD, &SLOTS, 1, &mut save));
        save.flags = [0xFF; crate::save::SAVE_FLAG_BYTES];
        logic.init_from_records(&WORLD);
        logic.restore_persistent(&WORLD, &SLOTS, &save);
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

    // ---- Timers and hazards ----

    /// Names: 1 = timer, 2 = door, 3 = gate, 4 = hazard.
    const fn timer(on: u16, off: u16, phase: u16, running: bool) -> LevelLogicRecord {
        LevelLogicRecord {
            targetname: 1,
            target: 2,
            arg0: on,
            arg1: off,
            arg2: phase,
            flags: logic_flags::ENABLED | if running { logic_flags::START_ON } else { 0 },
            ..blank(logic_kind::TIMER)
        }
    }

    /// The door's open state at every tick of `from..to`, as a string of
    /// `#` (open) and `.` (closed).
    fn door_trace(
        logic: &mut TestLogic,
        records: &'static [LevelLogicRecord],
        from: u32,
        to: u32,
    ) -> String {
        let mut out = String::new();
        for now in from..to {
            logic.tick(records, input_at([0, 0, 0]), now);
            out.push(if logic.door_open(1) { '#' } else { '.' });
        }
        out
    }

    #[test]
    fn a_running_timer_opens_and_closes_its_door_on_the_period() {
        static RECORDS: [LevelLogicRecord; 2] = [
            timer(3, 2, 0, true),
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        // The cycle begins at the first tick, whatever the clock says.
        assert_eq!(door_trace(&mut logic, &RECORDS, 100, 112), "###..###..##");
    }

    #[test]
    fn a_timers_phase_shifts_where_in_the_cycle_it_starts() {
        // 3 on, 2 off, phase 3: it starts at the beginning of the off stretch.
        static OFF_FIRST: [LevelLogicRecord; 2] = [
            timer(3, 2, 3, true),
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&OFF_FIRST);
        assert_eq!(door_trace(&mut logic, &OFF_FIRST, 0, 10), "..###..###");
        // Phase 1 is one tick into the on stretch; a phase past the period wraps.
        static LATE: [LevelLogicRecord; 2] = [
            timer(3, 2, 6, true),
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        logic.init_from_records(&LATE);
        // Phase 6 mod 5 is phase 1: two ticks of on left, then two off.
        assert_eq!(door_trace(&mut logic, &LATE, 0, 6), "##..##");
    }

    #[test]
    fn a_timer_that_starts_off_waits_for_a_use_then_stops_with_an_off() {
        static RECORDS: [LevelLogicRecord; 2] = [
            timer(3, 2, 0, false),
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        assert_eq!(
            door_trace(&mut logic, &RECORDS, 0, 5),
            ".....",
            "not running"
        );
        assert!(!logic.timer_running(0));
        // A use starts it from the beginning of its cycle, on that tick.
        logic.fire_by_name(&RECORDS, 1, UseCode::On, 5);
        assert!(logic.timer_running(0));
        assert!(logic.door_open(1), "the first On goes out at once");
        assert_eq!(door_trace(&mut logic, &RECORDS, 6, 12), "##..##");
        // Off stops it and closes what it drove.
        logic.fire_by_name(&RECORDS, 1, UseCode::Off, 12);
        assert!(!logic.timer_running(0));
        assert!(!logic.door_open(1));
        assert_eq!(door_trace(&mut logic, &RECORDS, 13, 20), ".......");
        // A toggle starts it again, from the beginning.
        logic.fire_by_name(&RECORDS, 1, UseCode::Toggle, 20);
        assert!(logic.door_open(1));
    }

    #[test]
    fn a_death_rearms_a_timer_to_the_same_beginning() {
        static RECORDS: [LevelLogicRecord; 2] = [
            timer(3, 2, 0, true),
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let first = door_trace(&mut logic, &RECORDS, 100, 104);
        assert_eq!(first, "###.");
        // The player dies mid-cycle and the world re-arms at tick 500.
        logic.init_from_records(&RECORDS);
        assert!(
            !logic.door_open(1),
            "the door is back to its authored state"
        );
        assert_eq!(door_trace(&mut logic, &RECORDS, 501, 505), first);
    }

    #[test]
    fn a_timer_sends_explicit_on_and_off_so_a_multisource_never_drifts() {
        // A required-1 gate driven by the timer: satisfied exactly while the
        // timer is on, however many cycles have gone by.
        static RECORDS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                target: 3,
                ..timer(2, 2, 0, true)
            },
            LevelLogicRecord {
                targetname: 3,
                arg0: 1,
                ..blank(logic_kind::MULTISOURCE)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let mut seen = String::new();
        for now in 0..12 {
            logic.tick(&RECORDS, input_at([0, 0, 0]), now);
            seen.push(if logic.is_on(&RECORDS, 1) { '#' } else { '.' });
        }
        assert_eq!(seen, "##..##..##..");
    }

    #[test]
    fn a_timer_delay_shifts_its_outputs_without_changing_its_own_cycle() {
        static RECORDS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                delay_ticks: 2,
                ..timer(3, 3, 0, true)
            },
            LevelLogicRecord {
                targetname: 2,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        assert_eq!(door_trace(&mut logic, &RECORDS, 0, 12), "..###...###.");
        // The timer itself ran on its own cycle, on for ticks 0..3.
    }

    /// Names: 4 = hazard, 5 = master gate.
    const fn hazard(damage: u16, interval: u16, on: bool) -> LevelLogicRecord {
        LevelLogicRecord {
            targetname: 4,
            arg0: damage,
            arg1: interval,
            min: [-50, 0, -50],
            max: [50, 100, 50],
            flags: logic_flags::ENABLED | if on { logic_flags::START_ON } else { 0 },
            ..blank(logic_kind::HAZARD)
        }
    }

    fn damage_over(
        logic: &mut TestLogic,
        records: &'static [LevelLogicRecord],
        player: [i32; 3],
        invulnerable: bool,
        ticks: core::ops::Range<u32>,
    ) -> Vec<u16> {
        let mut out = Vec::new();
        for now in ticks {
            logic.tick(
                records,
                LogicTickInput {
                    player,
                    player_invulnerable: invulnerable,
                },
                now,
            );
            out.push(logic.take_hazard_damage());
        }
        out
    }

    #[test]
    fn a_hazard_hits_on_contact_then_once_per_interval_while_the_player_stays() {
        static RECORDS: [LevelLogicRecord; 1] = [hazard(7, 4, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let inside = [0, 10, 0];
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 10..22),
            [7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0]
        );
        // Outside the volume nothing happens, and the damage is taken once.
        assert_eq!(
            damage_over(&mut logic, &RECORDS, [500, 10, 0], false, 22..26),
            [0; 4]
        );
        assert_eq!(logic.take_hazard_damage(), 0);
    }

    #[test]
    fn a_hazard_only_hurts_while_it_is_on() {
        static RECORDS: [LevelLogicRecord; 1] = [hazard(5, 1, false)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let inside = [0, 10, 0];
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 0..3),
            [0; 3]
        );
        assert!(!logic.is_on(&RECORDS, 0));
        logic.fire_by_name(&RECORDS, 4, UseCode::On, 3);
        assert!(logic.is_on(&RECORDS, 0));
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 3..6),
            [5; 3]
        );
        logic.fire_by_name(&RECORDS, 4, UseCode::Off, 6);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 6..9),
            [0; 3]
        );
        // Toggle flips it, and a second On does not stack.
        logic.fire_by_name(&RECORDS, 4, UseCode::Toggle, 9);
        logic.fire_by_name(&RECORDS, 4, UseCode::On, 9);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 9..11),
            [5; 2]
        );
    }

    #[test]
    fn invulnerable_frames_pass_through_a_hazard_and_do_not_spend_its_interval() {
        static RECORDS: [LevelLogicRecord; 1] = [hazard(9, 30, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let inside = [0, 10, 0];
        // A dash lasting 6 ticks through the beam: nothing lands.
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, true, 0..6),
            [0; 6]
        );
        // The first vulnerable tick inside is hit at once, not 30 ticks late.
        assert_eq!(
            damage_over(&mut logic, &RECORDS, inside, false, 6..8),
            [9, 0]
        );
    }

    #[test]
    fn a_timer_switches_a_hazard_with_on_and_off_directly() {
        // Beam on for 4 ticks, off for 4, damaging every tick it is on.
        static RECORDS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                target: 4,
                ..timer(4, 4, 0, true)
            },
            hazard(3, 1, false),
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, [0, 10, 0], false, 0..12),
            [3, 3, 3, 3, 0, 0, 0, 0, 3, 3, 3, 3]
        );
        // Standing clear of the volume through every phase costs nothing.
        logic.init_from_records(&RECORDS);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, [500, 10, 0], false, 0..12),
            [0; 12]
        );
    }

    #[test]
    fn a_master_multisource_gates_a_hazard() {
        // The timer feeds a gate; the hazard hurts only while the gate is
        // satisfied, though the hazard itself is always on.
        static RECORDS: [LevelLogicRecord; 3] = [
            LevelLogicRecord {
                target: 5,
                ..timer(2, 2, 0, true)
            },
            LevelLogicRecord {
                targetname: 5,
                arg0: 1,
                ..blank(logic_kind::MULTISOURCE)
            },
            LevelLogicRecord {
                master: 5,
                ..hazard(2, 1, true)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, [0, 10, 0], false, 0..8),
            [2, 2, 0, 0, 2, 2, 0, 0]
        );
        assert!(!logic.is_on(&RECORDS, 2), "gated off now");
    }

    #[test]
    fn a_hazard_can_kill_through_stacked_hits_and_rearms_with_the_world() {
        static RECORDS: [LevelLogicRecord; 1] = [hazard(40, 10, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let hits: u16 = damage_over(&mut logic, &RECORDS, [0, 10, 0], false, 0..30)
            .iter()
            .sum();
        assert_eq!(hits, 120, "three hits in thirty ticks");
        // Re-arm: the interval clock is fresh, so contact hits at once.
        logic.init_from_records(&RECORDS);
        assert_eq!(
            damage_over(&mut logic, &RECORDS, [0, 10, 0], false, 31..32),
            [40]
        );
    }

    // ---- A timer's visual and its hazard never part ----

    /// Three gates like the laser tunnel's: timers (names 11..13, 90 on and
    /// 90 off, phases 0, 90, 15) each switching their own hazard (names
    /// 21..23, a thin 40 deep volume).
    const fn gate_timer(name: u16, phase: u16) -> LevelLogicRecord {
        LevelLogicRecord {
            targetname: name,
            target: name + 10,
            ..timer(90, 90, phase, true)
        }
    }

    const fn gate_hazard(name: u16, z: i32, on: bool) -> LevelLogicRecord {
        LevelLogicRecord {
            targetname: name,
            min: [-500, 0, z - 20],
            max: [500, 300, z + 20],
            ..hazard(5, 1, on)
        }
    }

    static GATES: [LevelLogicRecord; 6] = [
        gate_timer(11, 0),
        gate_hazard(21, 1000, true),
        gate_timer(12, 90),
        gate_hazard(22, 2000, false),
        gate_timer(13, 15),
        gate_hazard(23, 3000, true),
    ];

    /// What the owner draws and what the hazard does come from the same
    /// state: a gate's visual (the timer's `is_on`) and its hazard (`is_on`,
    /// and the damage it deals to someone standing in it) agree on every tick
    /// of many cycles, through a re-arm and a switch-off of the timer.
    #[test]
    fn a_timers_visual_and_its_hazard_agree_on_every_tick_across_cycles_and_a_rearm() {
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&GATES);
        let z = [1000, 2000, 3000];
        let mut hurt_ticks = 0u32;
        for now in 0..2000u32 {
            // A re-arm (death, checkpoint rest) restarts every cycle.
            if now == 777 || now == 1500 {
                logic.init_from_records(&GATES);
            }
            for (gate, z) in z.iter().enumerate() {
                let player = [0, 10, *z];
                logic.tick(
                    &GATES,
                    LogicTickInput {
                        player,
                        player_invulnerable: false,
                    },
                    now,
                );
                let visual = logic.is_on(&GATES, gate * 2);
                let hazard = logic.is_on(&GATES, gate * 2 + 1);
                assert_eq!(visual, hazard, "gate {gate} diverged at tick {now}");
                // Only this gate's hazard can reach the player at its z.
                let damage = logic.take_hazard_damage();
                if hazard {
                    assert_eq!(damage, 5, "gate {gate} on at {now} must hurt");
                    hurt_ticks += 1;
                } else {
                    assert_eq!(damage, 0, "gate {gate} off at {now} must not hurt");
                }
            }
        }
        assert!(hurt_ticks > 1000, "the gates were on for many ticks");
    }

    // ---- Hazard knockback ----

    const fn pushing_hazard(damage: u16, knockback: u16, on: bool) -> LevelLogicRecord {
        LevelLogicRecord {
            arg2: knockback,
            // A gate: 100 wide, 40 thin in z.
            min: [-50, 0, -20],
            max: [50, 100, 20],
            ..hazard(damage, 30, on)
        }
    }

    fn tick_at(
        logic: &mut TestLogic,
        records: &'static [LevelLogicRecord],
        player: [i32; 3],
        invulnerable: bool,
        now: u32,
    ) -> (u16, Option<HazardPush>) {
        logic.tick(
            records,
            LogicTickInput {
                player,
                player_invulnerable: invulnerable,
            },
            now,
        );
        (logic.take_hazard_damage(), logic.take_hazard_push())
    }

    #[test]
    fn a_hazard_with_a_knockback_pushes_a_toucher_out_the_nearer_face_and_hurts_a_little() {
        static RECORDS: [LevelLogicRecord; 1] = [pushing_hazard(20, 12, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        // Walked in from the -z side, 5 units past the face at z -20.
        let (damage, push) = tick_at(&mut logic, &RECORDS, [0, 10, -15], false, 0);
        assert_eq!(damage, 20);
        assert_eq!(
            push,
            Some(HazardPush {
                axis_z: true,
                toward_max: false,
                tie: false,
                // 5 to the face, 12 past it, 1 to be clear of the inclusive box.
                distance: 18,
            })
        );
        // From the +z side it goes the other way.
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let (_, push) = tick_at(&mut logic, &RECORDS, [0, 10, 14], false, 0);
        assert_eq!(push.map(|p| (p.toward_max, p.distance)), Some((true, 19)));
        // Dead centre is a tie the owner breaks.
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let (_, push) = tick_at(&mut logic, &RECORDS, [0, 10, 0], false, 0);
        assert!(push.is_some_and(|p| p.tie));
    }

    #[test]
    fn a_hazard_switching_on_around_the_player_pushes_and_hurts_exactly_once() {
        // A timer switches it on at tick 4; the player stands inside from the start.
        static RECORDS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                target: 4,
                ..timer(4, 4, 4, true)
            },
            pushing_hazard(25, 10, false),
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        let inside = [0, 10, -10];
        for now in 0..4 {
            assert_eq!(
                tick_at(&mut logic, &RECORDS, inside, false, now),
                (0, None),
                "off: nothing at {now}"
            );
        }
        let (damage, push) = tick_at(&mut logic, &RECORDS, inside, false, 4);
        assert_eq!(damage, 25, "exactly the authored damage");
        assert!(push.is_some_and(|p| !p.toward_max && p.distance == 21));
        // The interval keeps it from damaging again straight away, but a
        // volume with a knockback still shoves anyone who stays inside.
        let (damage, push) = tick_at(&mut logic, &RECORDS, inside, false, 5);
        assert_eq!(damage, 0);
        assert!(push.is_some());
    }

    #[test]
    fn a_hazard_turning_on_with_the_player_outside_does_nothing() {
        static RECORDS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                target: 4,
                ..timer(4, 4, 0, true)
            },
            pushing_hazard(25, 10, false),
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        for now in 0..16 {
            assert_eq!(
                tick_at(&mut logic, &RECORDS, [0, 10, -200], false, now),
                (0, None)
            );
            assert!(logic.is_on(&RECORDS, 1) == (now % 8 < 4));
        }
    }

    #[test]
    fn an_invulnerable_player_is_not_pushed_and_a_hazard_without_a_knockback_never_pushes() {
        static RECORDS: [LevelLogicRecord; 1] = [pushing_hazard(20, 12, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RECORDS);
        assert_eq!(
            tick_at(&mut logic, &RECORDS, [0, 10, -15], true, 0),
            (0, None)
        );
        static PLAIN: [LevelLogicRecord; 1] = [pushing_hazard(20, 0, true)];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&PLAIN);
        assert_eq!(
            tick_at(&mut logic, &PLAIN, [0, 10, -15], false, 0),
            (20, None)
        );
    }
}
