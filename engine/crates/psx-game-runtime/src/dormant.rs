//! What a spawn remembers while its room is not in memory.
//!
//! An enemy that lives in a streamed room is only an entity while the room is
//! active. When the room leaves, the entity is dropped and the one or two
//! facts that must survive (it was killed, a flag it set) go into this
//! table, keyed by the entity's stable spawn id. A resident table beside the
//! chunks, not data inside them: a chunk is evicted and read again from the
//! disc, the table is not. The table is sorted by id, so a lookup is a
//! binary search and a world of thousands of spawns costs a few bytes each.

/// Bits of a dormant state byte.
pub mod dormant_state {
    /// The spawn was killed and stays dead.
    pub const DEAD: u8 = 1;
    /// First bit a game may use for its own per-spawn flags.
    pub const GAME_FIRST: u8 = 1 << 1;
}

/// A fixed-capacity table of per-spawn state, keyed by stable id.
pub struct DormantStates<const CAP: usize> {
    ids: [u32; CAP],
    states: [u8; CAP],
    len: u16,
    /// States that did not fit.
    overflow: u16,
}

impl<const CAP: usize> DormantStates<CAP> {
    /// An empty table, `const` so a game can keep it in zeroed storage.
    pub const EMPTY: Self = Self {
        ids: [0; CAP],
        states: [0; CAP],
        len: 0,
        overflow: 0,
    };

    /// Forget everything (a new game).
    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// States dropped because the table was full: not zero means `CAP` is
    /// too small for this world.
    pub fn overflow(&self) -> usize {
        usize::from(self.overflow)
    }

    fn find(&self, id: u32) -> Result<usize, usize> {
        self.ids[..self.len()].binary_search(&id)
    }

    /// The remembered state of `id`; zero (alive, no flags) when none.
    pub fn get(&self, id: psx_level::StableId) -> u8 {
        match self.find(id.raw()) {
            Ok(at) => self.states[at],
            Err(_) => 0,
        }
    }

    /// Add `bits` to the state of `id`. False when the table is full and
    /// the id is new.
    pub fn set(&mut self, id: psx_level::StableId, bits: u8) -> bool {
        if id.is_none() {
            return false;
        }
        match self.find(id.raw()) {
            Ok(at) => {
                self.states[at] |= bits;
                true
            }
            Err(at) => {
                let len = self.len();
                if len >= CAP {
                    self.overflow = self.overflow.saturating_add(1);
                    return false;
                }
                self.ids.copy_within(at..len, at + 1);
                self.states.copy_within(at..len, at + 1);
                self.ids[at] = id.raw();
                self.states[at] = bits;
                self.len += 1;
                true
            }
        }
    }

    /// The remembered entries as `(id, state)`, ascending by id: what a
    /// save writes.
    pub fn iter(&self) -> impl Iterator<Item = (psx_level::StableId, u8)> + '_ {
        (0..self.len()).map(|i| (psx_level::StableId(self.ids[i]), self.states[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::StableId;

    #[test]
    fn states_are_kept_sorted_and_found_by_id() {
        let mut table = DormantStates::<4>::EMPTY;
        let ids = [StableId(900), StableId(5), StableId(300)];
        for id in ids {
            assert!(table.set(id, dormant_state::DEAD));
        }
        assert_eq!(table.len(), 3);
        let order: [u32; 3] = {
            let mut it = table.iter().map(|(id, _)| id.raw());
            [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(order, [5, 300, 900]);
        assert_eq!(table.get(StableId(300)), dormant_state::DEAD);
        assert_eq!(table.get(StableId(301)), 0);
        // Bits add up; a full table counts what it dropped.
        assert!(table.set(StableId(300), dormant_state::GAME_FIRST));
        assert_eq!(table.get(StableId(300)), 3);
        assert!(table.set(StableId(7), 1));
        assert!(!table.set(StableId(8), 1));
        assert_eq!(table.overflow(), 1);
        assert!(!table.set(StableId::NONE, 1));
    }
}
