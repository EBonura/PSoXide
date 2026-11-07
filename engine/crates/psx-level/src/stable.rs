//! Stable global ids for authored things: enemy spawns, logic records,
//! checkpoints.
//!
//! A table index changes whenever the cook reorders, adds or removes
//! records, and once records live in per-room (streamed) sections an index
//! no longer names anything. A stable id is a 32-bit FNV-1a hash of an
//! authored identity that survives recooks: the node id of the authored
//! thing, or the checkpoint's own name. Saves, per-spawn state and cross
//! references use these ids and never an array index. The cook refuses a
//! level whose ids collide.

/// A stable id. Zero is reserved: it means "none".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StableId(pub u32);

impl StableId {
    /// No id.
    pub const NONE: Self = Self(0);

    /// The id of an authored name (a checkpoint id, a node name).
    pub const fn of_name(name: &str) -> Self {
        Self::of_bytes(name.as_bytes())
    }

    /// The id of an authored node, from its raw node id given as its
    /// `[low, high]` 32-bit words (the runtime has no 64-bit integers; the
    /// host splits the id).
    pub const fn of_node(raw_node_id: [u32; 2]) -> Self {
        Self::of_node_part(raw_node_id, 0)
    }

    /// The id of the `part`-th record one authored node cooks to (an
    /// interactable cooks a placed record and a paired logic record).
    pub const fn of_node_part(raw_node_id: [u32; 2], part: u8) -> Self {
        let lo = raw_node_id[0].to_le_bytes();
        let hi = raw_node_id[1].to_le_bytes();
        Self::of_bytes(&[lo[0], lo[1], lo[2], lo[3], hi[0], hi[1], hi[2], hi[3], part])
    }

    const fn of_bytes(bytes: &[u8]) -> Self {
        let mut hash = 0x811c_9dc5u32;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u32;
            hash = hash.wrapping_mul(0x0100_0193);
            i += 1;
        }
        // Zero is "none": the one hash that lands on it moves to one.
        Self(if hash == 0 { 1 } else { hash })
    }

    /// Raw value for storage.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Whether this is the reserved "none".
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Position of `id` in a table of ids, if present.
pub fn position_of(ids: &[StableId], id: StableId) -> Option<usize> {
    if id.is_none() {
        return None;
    }
    ids.iter().position(|&candidate| candidate == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_come_from_the_authored_identity_not_the_order() {
        assert_eq!(StableId::of_name("altar"), StableId::of_name("altar"));
        assert_ne!(StableId::of_name("altar"), StableId::of_name("altar2"));
        assert_ne!(StableId::of_node([7, 0]), StableId::of_node([8, 0]));
        assert_ne!(
            StableId::of_node_part([7, 0], 0),
            StableId::of_node_part([7, 0], 1)
        );
        assert_eq!(StableId::of_node([7, 0]), StableId::of_node_part([7, 0], 0));
        assert!(!StableId::of_node([0, 0]).is_none());
        let ids = [StableId::of_node([3, 0]), StableId::of_node([9, 0])];
        assert_eq!(position_of(&ids, StableId::of_node([9, 0])), Some(1));
        assert_eq!(position_of(&ids, StableId::NONE), None);
        // The two 32-bit words hash as the little-endian bytes of the old
        // 64-bit id, so every id cooked before the split is unchanged.
        assert_eq!(
            StableId::of_node_part([0x0403_0201, 0x0807_0605], 9),
            StableId::of_bytes(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        );
        // The hash is FNV-1a: a pinned value so a change cannot slip by.
        assert_eq!(StableId::of_name("").raw(), 0x811c_9dc5);
        assert_eq!(StableId::of_name("a").raw(), 0xe40c_292c);
    }
}
