//! Slot ownership: entities are spawned into slots one at a time and named
//! by a stable spawn id, so the live set can be a pool the streamer fills and
//! empties as rooms come and go, not a mirror of one global record slice.

use super::*;
use crate::dormant::{dormant_state, DormantStates};
use psx_level::StableId;

impl<const N: usize, const S: bool> GameEntities<N, S> {
    /// Initialise slot `index` from `record`. `seed` seeds the entity's
    /// tactical randomness.
    // Shared by every spawn path; keep one copy on PS1.
    #[inline(never)]
    pub(super) fn init_slot(&mut self, index: usize, record: &LevelGameEntityRecord, seed: u32) {
        self.tactics[index].seed = seed;
        self.x[index] = record.x;
        self.y[index] = record.y;
        self.z[index] = record.z;
        self.yaw[index] = record.yaw;
        self.health[index] = record.max_health;
        self.health_secondary[index] = record.max_health_secondary;
        self.poise[index] = crate::poise::Poise::EMPTY;
        self.flow[index] = crate::combat_flow::CombatFlow::FULL;
        self.exchanges[index] = crate::ranged_tactics::RangedExchange::EMPTY;
        self.projectile_threats[index] = None;
        self.recoil[index] = [0; 3];
        self.stance_pulse[index] = 0;
        self.patrol_leg[index] = 0;
        self.move_yaw[index] = 0;
        self.move_yaw_valid[index] = 0;
        self.move_tried[index] = 0;
        self.state_ticks[index] = 0;
        self.turn_ticks[index] = 0;
        self.turn_phase_ticks[index] = 0;
        self.intent[index] = GameEntityIntent::Hold as u8;
        self.attack_cooldown[index] = 0;
        self.attack_wait_ticks[index] = 0;
        self.attack_mode[index] = GAME_ENTITY_ATTACK_LIGHT;
        self.set_stance_swap_elapsed(index, GAME_ENTITY_STANCE_SWAP_DURATION_TICKS);
        let enabled = record.flags & game_entity_flags::ENABLED != 0;
        self.state[index] = if enabled {
            GameEntityState::Idle as u8
        } else {
            GameEntityState::Dead as u8
        };
    }

    /// Like [`Self::spawn_from_records`], naming slot `i` by `ids[i]`
    /// (slots past the end of `ids` have no id).
    #[inline(never)]
    pub fn spawn_from_records_with_ids(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        ids: &[StableId],
    ) {
        self.spawn_from_records(records);
        for (slot, id) in ids.iter().enumerate().take(self.count()) {
            self.spawn_ids[slot] = id.raw();
        }
    }

    /// Spawn `record` into `slot` under the stable id `id`, replacing
    /// whatever the slot held. False when `slot` is past the pool. Its
    /// tactical randomness comes from the id, so an entity behaves the same
    /// whichever slot it lands in.
    pub fn spawn_slot(
        &mut self,
        slot: usize,
        id: StableId,
        record: &LevelGameEntityRecord,
    ) -> bool {
        if slot >= N || slot >= usize::from(u16::MAX) {
            return false;
        }
        self.init_slot(slot, record, 0x9e3779b9u32.wrapping_add(id.raw()));
        self.spawn_ids[slot] = id.raw();
        self.count = self.count.max(slot as u16 + 1);
        true
    }

    /// Spawn into `slot`, honouring what the dormant table remembers: an
    /// entity that was killed before it left spawns dead and stays dead.
    pub fn spawn_slot_remembered<const D: usize>(
        &mut self,
        slot: usize,
        id: StableId,
        record: &LevelGameEntityRecord,
        dormant: &DormantStates<D>,
    ) -> bool {
        if !self.spawn_slot(slot, id, record) {
            return false;
        }
        if dormant.get(id) & dormant_state::DEAD != 0 {
            self.state[slot] = GameEntityState::Dead as u8;
            self.health[slot] = 0;
            self.health_secondary[slot] = 0;
        }
        true
    }

    /// Take the entity out of `slot` (its room left the active set),
    /// writing what must outlive it into the dormant table. The slot is left
    /// dead and nameless, free for the next spawn.
    pub fn release_slot<const D: usize>(&mut self, slot: usize, dormant: &mut DormantStates<D>) {
        if slot >= N {
            return;
        }
        let id = StableId(self.spawn_ids[slot]);
        if !id.is_none() && self.state[slot] == GameEntityState::Dead as u8 {
            dormant.set(id, dormant_state::DEAD);
        }
        self.state[slot] = GameEntityState::Dead as u8;
        self.spawn_ids[slot] = 0;
    }

    /// The stable spawn id of the entity in `slot`, if it has one.
    pub fn spawn_id(&self, slot: usize) -> StableId {
        StableId(self.spawn_ids.get(slot).copied().unwrap_or(0))
    }

    /// The slot holding the entity named `id`.
    pub fn slot_of(&self, id: StableId) -> Option<usize> {
        if id.is_none() {
            return None;
        }
        self.spawn_ids[..self.count()]
            .iter()
            .position(|&candidate| candidate == id.raw())
    }
}
