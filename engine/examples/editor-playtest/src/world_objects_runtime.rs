//! Shared runtime visibility over the cooker's typed world-object registry.
//!
//! Geometry remains in compact specialist tables, but those renderers must
//! ask this mask before submitting packets. PXBSP fills it from leaf PVS plus
//! direct brush traces; grid worlds use the all-visible value and retain their
//! room/portal filtering.

use psx_level::{LevelWorldObjectRecord, MAX_WORLD_OBJECTS};
use psx_game_runtime::destructibles::RuntimeDestructibles;

pub(super) const WORLD_OBJECT_VISIBILITY_WORDS: usize =
    MAX_WORLD_OBJECTS.div_ceil(u64::BITS as usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WorldObjectVisibility {
    words: [u64; WORLD_OBJECT_VISIBILITY_WORDS],
}

impl WorldObjectVisibility {
    pub(super) const NONE: Self = Self {
        words: [0; WORLD_OBJECT_VISIBILITY_WORDS],
    };

    pub(super) const ALL: Self = Self {
        words: [u64::MAX; WORLD_OBJECT_VISIBILITY_WORDS],
    };

    pub(super) fn set(&mut self, index: usize) {
        if index >= MAX_WORLD_OBJECTS {
            return;
        }
        self.words[index / u64::BITS as usize] |= 1u64 << (index % u64::BITS as usize);
    }

    pub(super) const fn contains(self, index: usize) -> bool {
        if index >= MAX_WORLD_OBJECTS {
            return false;
        }
        self.words[index / u64::BITS as usize] & (1u64 << (index % u64::BITS as usize)) != 0
    }

    /// Resolve a typed payload through the shared registry. A missing record
    /// fails closed in BSP play: malformed cooking must not re-open the old
    /// renderer-specific visibility bypass.
    pub(super) fn typed_visible(
        self,
        objects: &[LevelWorldObjectRecord],
        kind: u8,
        source_index: usize,
    ) -> bool {
        let Ok(source_index) = u16::try_from(source_index) else {
            return false;
        };
        // The cooker emits typed groups in kind/source order. Binary search
        // keeps specialist render passes from rescanning the complete registry
        // once for every prop in a dense level.
        objects
            .binary_search_by_key(&(kind, source_index), |object| {
                (object.kind, object.source_index)
            })
            .is_ok_and(|index| self.contains(index))
    }
}

const _: () = assert!(WORLD_OBJECT_VISIBILITY_WORDS == 2);

/// Whether one typed payload remains live according to the same shared state
/// used by brush submodels. Works for BSP and legacy-grid scenes alike.
pub(super) fn typed_world_object_active(
    objects: &[LevelWorldObjectRecord],
    destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    kind: u8,
    source_index: usize,
) -> bool {
    let Ok(source_index) = u16::try_from(source_index) else {
        return false;
    };
    objects
        .binary_search_by_key(&(kind, source_index), |object| {
            (object.kind, object.source_index)
        })
        .ok()
        .and_then(|index| objects.get(index))
        .is_some_and(|object| {
            object.destructible == psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
                || destructibles.alive(usize::from(object.destructible))
        })
}
