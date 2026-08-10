//! Fixed-tick motion state for PXBSP brush submodels.

use crate::collision::BrushTransform;
use crate::pxbsp::{
    entity_class, entity_flags, PxbspBrushDoor, PxbspBrushDoorError, PxbspEntity, PxbspEntityTable,
};
use crate::pxbsp_resident::PxbspResidentMap;
use crate::{Vec3I16, Vec3I32};

/// Invalid entity/payload combination for one translated brush door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushDoorError {
    WrongClass(u16),
    UnknownFlags(u16),
    InvalidModel(u16),
    UnsupportedAngles(Vec3I16),
    InvalidPayload(PxbspBrushDoorError),
}

/// Runtime state for one linearly translated brush door.
///
/// The state stores an integer progress tick, so reversing direction never
/// accumulates interpolation error. Render and collision consume the same
/// [`BrushTransform`] returned by [`Self::transform`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushDoor {
    model_index: u16,
    closed_origin: Vec3I32,
    open_offset: Vec3I32,
    travel_ticks: u16,
    progress_ticks: u16,
    target_open: bool,
    enabled: bool,
}

impl BrushDoor {
    const EMPTY: Self = Self {
        model_index: 0,
        closed_origin: Vec3I32 { x: 0, y: 0, z: 0 },
        open_offset: Vec3I32 { x: 0, y: 0, z: 0 },
        travel_ticks: 1,
        progress_ticks: 0,
        target_open: false,
        enabled: false,
    };

    /// Build motion state from one validated PXBSP entity and typed payload.
    pub fn from_entity(
        entity: PxbspEntity,
        payload: PxbspBrushDoor,
    ) -> Result<Self, BrushDoorError> {
        if entity.class_id != entity_class::BRUSH_DOOR {
            return Err(BrushDoorError::WrongClass(entity.class_id));
        }
        if entity.flags & !entity_flags::KNOWN != 0 {
            return Err(BrushDoorError::UnknownFlags(entity.flags));
        }
        if entity.model == 0 || entity.model == u16::MAX {
            return Err(BrushDoorError::InvalidModel(entity.model));
        }
        if entity.angles != (Vec3I16 { x: 0, y: 0, z: 0 }) {
            return Err(BrushDoorError::UnsupportedAngles(entity.angles));
        }
        payload.validate().map_err(BrushDoorError::InvalidPayload)?;
        let target_open = entity.flags & entity_flags::START_OPEN != 0;
        Ok(Self {
            model_index: entity.model,
            closed_origin: entity.origin,
            open_offset: payload.open_offset,
            travel_ticks: payload.travel_ticks,
            progress_ticks: if target_open { payload.travel_ticks } else { 0 },
            target_open,
            enabled: entity.flags & entity_flags::ENABLED != 0,
        })
    }

    pub const fn model_index(self) -> usize {
        self.model_index as usize
    }

    pub const fn enabled(self) -> bool {
        self.enabled
    }

    pub const fn target_open(self) -> bool {
        self.target_open
    }

    pub const fn fully_open(self) -> bool {
        self.progress_ticks == self.travel_ticks
    }

    pub const fn fully_closed(self) -> bool {
        self.progress_ticks == 0
    }

    /// Change the target endpoint without changing current progress.
    pub fn set_open(&mut self, open: bool) {
        if self.enabled {
            self.target_open = open;
        }
    }

    pub fn toggle(&mut self) {
        self.set_open(!self.target_open);
    }

    /// Advance exactly one 60 Hz simulation tick.
    ///
    /// Returns `true` when the transform changed.
    pub fn tick(&mut self) -> bool {
        let before = self.progress_ticks;
        if self.enabled {
            if self.target_open {
                self.progress_ticks = self.progress_ticks.saturating_add(1).min(self.travel_ticks);
            } else {
                self.progress_ticks = self.progress_ticks.saturating_sub(1);
            }
        }
        self.progress_ticks != before
    }

    /// Current model-local to world transform for render and collision.
    pub fn transform(self) -> BrushTransform {
        BrushTransform::translated(Vec3I32 {
            x: endpoint_axis(
                self.closed_origin.x,
                self.open_offset.x,
                self.progress_ticks,
                self.travel_ticks,
            ),
            y: endpoint_axis(
                self.closed_origin.y,
                self.open_offset.y,
                self.progress_ticks,
                self.travel_ticks,
            ),
            z: endpoint_axis(
                self.closed_origin.z,
                self.open_offset.z,
                self.progress_ticks,
                self.travel_ticks,
            ),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushDoorSetError {
    CapacityExceeded {
        max: usize,
    },
    MissingPayload {
        entity: usize,
    },
    InvalidDoor {
        entity: usize,
        error: BrushDoorError,
    },
    MissingModel {
        entity: usize,
        model: u16,
    },
}

/// Fixed-capacity brush-door state discovered from a resident PXBSP map.
pub struct BrushDoorSet<const MAX_DOORS: usize> {
    count: usize,
    entity_indices: [u16; MAX_DOORS],
    doors: [BrushDoor; MAX_DOORS],
}

impl<const MAX_DOORS: usize> BrushDoorSet<MAX_DOORS> {
    pub const EMPTY: Self = Self {
        count: 0,
        entity_indices: [0; MAX_DOORS],
        doors: [BrushDoor::EMPTY; MAX_DOORS],
    };

    pub fn init_from_map(&mut self, map: &PxbspResidentMap) -> Result<(), BrushDoorSetError> {
        self.init_from_entities(map.entities(), map.brush_models().len())
    }

    pub fn init_from_entities(
        &mut self,
        entities: PxbspEntityTable<'_>,
        brush_model_count: usize,
    ) -> Result<(), BrushDoorSetError> {
        *self = Self::EMPTY;
        let result = (|| {
            for entity_index in 0..entities.len() {
                let entity = entities
                    .get(entity_index)
                    .expect("entity index is inside checked table");
                if entity.class_id != entity_class::BRUSH_DOOR {
                    continue;
                }
                if self.count == MAX_DOORS {
                    return Err(BrushDoorSetError::CapacityExceeded { max: MAX_DOORS });
                }
                let payload = entities
                    .payload_record::<PxbspBrushDoor>(entity_index)
                    .ok_or(BrushDoorSetError::MissingPayload {
                        entity: entity_index,
                    })?;
                let door = BrushDoor::from_entity(entity, payload).map_err(|error| {
                    BrushDoorSetError::InvalidDoor {
                        entity: entity_index,
                        error,
                    }
                })?;
                if door.model_index() >= brush_model_count {
                    return Err(BrushDoorSetError::MissingModel {
                        entity: entity_index,
                        model: entity.model,
                    });
                }
                self.entity_indices[self.count] = u16::try_from(entity_index).unwrap_or(u16::MAX);
                self.doors[self.count] = door;
                self.count += 1;
            }
            Ok(())
        })();
        if result.is_err() {
            *self = Self::EMPTY;
        }
        result
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn get(&self, index: usize) -> Option<&BrushDoor> {
        self.doors.get(..self.count)?.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut BrushDoor> {
        self.doors.get_mut(..self.count)?.get_mut(index)
    }

    pub fn entity_index(&self, index: usize) -> Option<usize> {
        self.entity_indices
            .get(..self.count)?
            .get(index)
            .copied()
            .map(usize::from)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &BrushDoor> {
        self.doors[..self.count].iter()
    }

    /// Advance all movers one 60 Hz tick and return the number that moved.
    pub fn tick(&mut self) -> usize {
        self.doors[..self.count]
            .iter_mut()
            .fold(0, |moved, door| moved + usize::from(door.tick()))
    }
}

impl<const MAX_DOORS: usize> Default for BrushDoorSet<MAX_DOORS> {
    fn default() -> Self {
        Self::EMPTY
    }
}

fn endpoint_axis(origin: i32, offset: i32, progress: u16, duration: u16) -> i32 {
    let delta = i64::from(offset) * i64::from(progress) / i64::from(duration);
    origin.saturating_add(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::pxbsp::PXBSP_ENTITY_TABLE_HEADER_BYTES;
    use crate::CookedRecord;

    const CLOSED: Vec3I32 = Vec3I32 {
        x: 100 * 4096,
        y: 20 * 4096,
        z: -40 * 4096,
    };

    fn entity(flags: u16) -> PxbspEntity {
        PxbspEntity {
            class_id: entity_class::BRUSH_DOOR,
            flags,
            model: 2,
            origin: CLOSED,
            ..PxbspEntity::default()
        }
    }

    fn payload() -> PxbspBrushDoor {
        PxbspBrushDoor::new(
            Vec3I32 {
                x: 8 * 4096,
                y: -4 * 4096,
                z: 0,
            },
            4,
        )
    }

    fn entity_table(inputs: &[(PxbspEntity, &[u8])]) -> alloc::vec::Vec<u8> {
        let records_end = PXBSP_ENTITY_TABLE_HEADER_BYTES + inputs.len() * PxbspEntity::SIZE;
        let payload_start = (records_end + 3) & !3;
        let mut bytes = vec![0; payload_start];
        bytes[0..2].copy_from_slice(&(inputs.len() as u16).to_le_bytes());
        bytes[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
        bytes[4..8].copy_from_slice(&(payload_start as u32).to_le_bytes());
        let mut payload_offset = 0usize;
        for (index, (entity, payload)) in inputs.iter().enumerate() {
            let record = PXBSP_ENTITY_TABLE_HEADER_BYTES + index * PxbspEntity::SIZE;
            bytes[record..record + 2].copy_from_slice(&entity.class_id.to_le_bytes());
            bytes[record + 2..record + 4].copy_from_slice(&entity.flags.to_le_bytes());
            bytes[record + 4..record + 6].copy_from_slice(&entity.model.to_le_bytes());
            bytes[record + 8..record + 12].copy_from_slice(&entity.origin.x.to_le_bytes());
            bytes[record + 12..record + 16].copy_from_slice(&entity.origin.y.to_le_bytes());
            bytes[record + 16..record + 20].copy_from_slice(&entity.origin.z.to_le_bytes());
            bytes[record + 26..record + 30].copy_from_slice(&(payload_offset as u32).to_le_bytes());
            bytes[record + 30..record + 32].copy_from_slice(&(payload.len() as u16).to_le_bytes());
            bytes.extend_from_slice(payload);
            payload_offset += payload.len();
        }
        bytes
    }

    #[test]
    fn advances_to_exact_endpoints_at_fixed_ticks() {
        let mut door =
            BrushDoor::from_entity(entity(entity_flags::ENABLED), payload()).expect("brush door");
        assert!(door.fully_closed());
        assert_eq!(door.transform().origin, CLOSED);
        door.set_open(true);
        for step in 1..=4 {
            assert!(door.tick());
            assert_eq!(
                door.transform().origin,
                Vec3I32 {
                    x: (100 + 2 * step) * 4096,
                    y: (20 - step) * 4096,
                    z: -40 * 4096,
                }
            );
        }
        assert!(door.fully_open());
        assert!(!door.tick());
    }

    #[test]
    fn reversal_reuses_integer_progress_without_drift() {
        let mut door =
            BrushDoor::from_entity(entity(entity_flags::ENABLED), payload()).expect("brush door");
        door.set_open(true);
        assert!(door.tick());
        assert!(door.tick());
        let midpoint = door.transform();
        door.set_open(false);
        assert!(door.tick());
        door.set_open(true);
        assert!(door.tick());
        assert_eq!(door.transform(), midpoint);
    }

    #[test]
    fn start_open_and_disabled_flags_are_honored() {
        let open = BrushDoor::from_entity(
            entity(entity_flags::ENABLED | entity_flags::START_OPEN),
            payload(),
        )
        .expect("open door");
        assert!(open.fully_open());

        let mut disabled = BrushDoor::from_entity(entity(0), payload()).expect("disabled door");
        disabled.set_open(true);
        assert!(!disabled.target_open());
        assert!(!disabled.tick());
        assert!(disabled.fully_closed());
    }

    #[test]
    fn rejects_world_model_rotation_and_unknown_flags() {
        let mut bad_model = entity(entity_flags::ENABLED);
        bad_model.model = 0;
        assert_eq!(
            BrushDoor::from_entity(bad_model, payload()),
            Err(BrushDoorError::InvalidModel(0))
        );

        let mut rotated = entity(entity_flags::ENABLED);
        rotated.angles.y = 1;
        assert!(matches!(
            BrushDoor::from_entity(rotated, payload()),
            Err(BrushDoorError::UnsupportedAngles(_))
        ));

        assert_eq!(
            BrushDoor::from_entity(entity(1 << 15), payload()),
            Err(BrushDoorError::UnknownFlags(1 << 15))
        );
    }

    #[test]
    fn set_discovers_ticks_and_maps_door_entities() {
        let first_payload = payload().to_le_bytes();
        let second_payload = PxbspBrushDoor::new(
            Vec3I32 {
                x: 0,
                y: 12 * 4096,
                z: 0,
            },
            3,
        )
        .to_le_bytes();
        let inert = PxbspEntity {
            class_id: 99,
            model: u16::MAX,
            ..PxbspEntity::default()
        };
        let mut second = entity(entity_flags::ENABLED | entity_flags::START_OPEN);
        second.model = 3;
        let bytes = entity_table(&[
            (inert, &[]),
            (entity(entity_flags::ENABLED), &first_payload),
            (second, &second_payload),
        ]);
        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        let mut doors = BrushDoorSet::<2>::default();
        doors.init_from_entities(table, 4).expect("door collection");
        assert_eq!(doors.len(), 2);
        assert_eq!(doors.entity_index(0), Some(1));
        assert_eq!(doors.entity_index(1), Some(2));
        assert!(doors.get(0).expect("first").fully_closed());
        assert!(doors.get(1).expect("second").fully_open());
        doors.get_mut(0).expect("first").set_open(true);
        doors.get_mut(1).expect("second").set_open(false);
        assert_eq!(doors.tick(), 2);
    }

    #[test]
    fn set_rejects_capacity_payload_and_model_failures() {
        let good_payload = payload().to_le_bytes();
        let one = entity(entity_flags::ENABLED);
        let mut two = entity(entity_flags::ENABLED);
        two.model = 3;
        let bytes = entity_table(&[(one, &good_payload), (two, &good_payload)]);
        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        let mut too_small = BrushDoorSet::<1>::default();
        assert_eq!(
            too_small.init_from_entities(table, 4),
            Err(BrushDoorSetError::CapacityExceeded { max: 1 })
        );
        assert!(too_small.is_empty());

        let bytes = entity_table(&[(one, &[1, 2, 3])]);
        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        assert_eq!(
            BrushDoorSet::<1>::default().init_from_entities(table, 4),
            Err(BrushDoorSetError::MissingPayload { entity: 0 })
        );

        let mut missing = one;
        missing.model = 9;
        let bytes = entity_table(&[(missing, &good_payload)]);
        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        assert_eq!(
            BrushDoorSet::<1>::default().init_from_entities(table, 4),
            Err(BrushDoorSetError::MissingModel {
                entity: 0,
                model: 9,
            })
        );
    }
}
