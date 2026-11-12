//! Logic-driven visibility: a plane, a mesh instance or a particle emitter
//! that is shown only while a named logic node is on (or off).
//!
//! The author names the node (a Timer, Door, Hazard, Switch or Multisource by
//! its node name). The cook gives every distinct name one slot, up to
//! [`psx_grid::visual_switch::SLOTS`] per level, in the order the names are
//! first met in the world (rooms, then planes, then meshes) and then in the
//! scene (particle emitters). The slot rides in the drawn record's one
//! `visual` byte; the level's `visual_links` table says which logic record
//! each slot follows. The runtime keeps one bit per slot.

use serde::{Deserialize, Serialize};

use super::world::GridWorld;

/// Shown while the named logic node is on (or, inverted, while it is off).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicVisibility {
    /// Name of the logic node that switches it: a Timer, Door, Hazard,
    /// Multisource, or a Switch interactable.
    pub source: String,
    /// Shown while the node is OFF instead of on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
}

impl LogicVisibility {
    /// Shown while `source` is on.
    pub fn while_on(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            invert: false,
        }
    }

    /// The trimmed source name, or `None` for an empty one (which switches
    /// nothing).
    pub fn name(&self) -> Option<&str> {
        let name = self.source.trim();
        (!name.is_empty()).then_some(name)
    }
}

/// The slots of one level's visual switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisualSlots {
    names: Vec<String>,
}

impl VisualSlots {
    /// The slots the world's planes and mesh instances need, in the order
    /// the cook meets them. Particle emitters are not part of the grid world;
    /// the playtest cook adds theirs with [`Self::slot_of`].
    pub fn from_world(world: &GridWorld) -> Self {
        let mut slots = Self::default();
        for room in &world.rooms {
            for plane in &room.planes {
                if let Some(visibility) = &plane.shown_by {
                    slots.slot_of(visibility);
                }
            }
            for mesh in &room.meshes {
                if let Some(visibility) = &mesh.shown_by {
                    slots.slot_of(visibility);
                }
            }
        }
        slots
    }

    /// The slot of `visibility`'s node, taking the next free one for a name
    /// not met before. `None` for an unnamed source.
    pub fn slot_of(&mut self, visibility: &LogicVisibility) -> Option<usize> {
        let name = visibility.name()?;
        if let Some(slot) = self.names.iter().position(|known| known == name) {
            return Some(slot);
        }
        self.names.push(name.to_string());
        Some(self.names.len() - 1)
    }

    /// The `visual` byte for a drawn record: zero when nothing switches it,
    /// otherwise the slot and inversion ([`psx_grid::visual_switch`]). A slot
    /// past the level's capacity also reads as zero; [`Self::overflow`]
    /// reports it as a cook error.
    pub fn flags(&mut self, visibility: Option<&LogicVisibility>) -> u8 {
        let Some(visibility) = visibility else {
            return 0;
        };
        match self.slot_of(visibility) {
            Some(slot) if slot < psx_grid::visual_switch::SLOTS => {
                psx_grid::visual_switch::encode(slot, visibility.invert)
            }
            _ => 0,
        }
    }

    /// The node name of every slot, in slot order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// A message when the level needs more slots than the runtime keeps.
    pub fn overflow(&self) -> Option<String> {
        (self.names.len() > psx_grid::visual_switch::SLOTS).then(|| {
            format!(
                "the level switches planes, meshes or emitters by {} different logic nodes, \
                 but the runtime keeps {} visual switches",
                self.names.len(),
                psx_grid::visual_switch::SLOTS
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::plane::GridPlane;
    use crate::grid::room::GridRoom;
    use psx_grid::visual_switch;

    fn world_with(planes: &[Option<LogicVisibility>]) -> GridWorld {
        let mut room = GridRoom::new("r", 4, 4);
        for shown_by in planes {
            let mut plane = GridPlane::new([0, 0, 0], [1, 1]);
            plane.shown_by = shown_by.clone();
            room.planes.push(plane);
        }
        let mut world = GridWorld::default();
        world.rooms.push(room);
        world
    }

    #[test]
    fn names_share_a_slot_and_slots_follow_first_use() {
        let beam = LogicVisibility::while_on("gate_a");
        let dark = LogicVisibility {
            source: "gate_a".into(),
            invert: true,
        };
        let other = LogicVisibility::while_on("gate_b");
        let world = world_with(&[
            None,
            Some(beam.clone()),
            Some(other.clone()),
            Some(dark.clone()),
        ]);
        let mut slots = VisualSlots::from_world(&world);
        assert_eq!(slots.names(), ["gate_a", "gate_b"]);
        assert_eq!(slots.flags(None), 0);
        assert_eq!(slots.flags(Some(&beam)), visual_switch::encode(0, false));
        assert_eq!(slots.flags(Some(&dark)), visual_switch::encode(0, true));
        assert_eq!(slots.flags(Some(&other)), visual_switch::encode(1, false));
        // An emitter naming a new node takes the next slot.
        let spark = LogicVisibility::while_on("gate_c");
        assert_eq!(slots.flags(Some(&spark)), visual_switch::encode(2, false));
        assert!(slots.overflow().is_none());
    }

    #[test]
    fn an_unnamed_source_switches_nothing() {
        let mut slots = VisualSlots::default();
        assert_eq!(slots.flags(Some(&LogicVisibility::while_on("  "))), 0);
        assert!(slots.names().is_empty());
    }

    #[test]
    fn more_nodes_than_slots_is_reported() {
        let planes: Vec<_> = (0..=visual_switch::SLOTS)
            .map(|i| Some(LogicVisibility::while_on(format!("n{i}"))))
            .collect();
        let slots = VisualSlots::from_world(&world_with(&planes));
        assert!(slots.overflow().is_some());
    }
}
