//! Damageable stationary PXBSP brush submodels.

use crate::collision::BrushTransform;
use crate::pxbsp::{
    entity_class, PxbspBrushDestructible, PxbspBrushDestructibleError, PxbspEntity,
    PxbspEntityTable,
};
use crate::pxbsp_resident::PxbspResidentMap;
use crate::{Vec3I16, Vec3I32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushDestructibleError {
    WrongClass(u16),
    UnknownFlags(u16),
    InvalidModel(u16),
    UnsupportedAngles(Vec3I16),
    InvalidPayload(PxbspBrushDestructibleError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushDestructible {
    model_index: u16,
    origin: Vec3I32,
    destructible_index: u16,
}

impl BrushDestructible {
    const EMPTY: Self = Self {
        model_index: 0,
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        destructible_index: 0,
    };

    pub fn from_entity(
        entity: PxbspEntity,
        payload: PxbspBrushDestructible,
    ) -> Result<Self, BrushDestructibleError> {
        if entity.class_id != entity_class::BRUSH_DESTRUCTIBLE {
            return Err(BrushDestructibleError::WrongClass(entity.class_id));
        }
        if entity.flags != 0 {
            return Err(BrushDestructibleError::UnknownFlags(entity.flags));
        }
        if entity.model == 0 || entity.model == u16::MAX {
            return Err(BrushDestructibleError::InvalidModel(entity.model));
        }
        if entity.angles != (Vec3I16 { x: 0, y: 0, z: 0 }) {
            return Err(BrushDestructibleError::UnsupportedAngles(entity.angles));
        }
        payload
            .validate()
            .map_err(BrushDestructibleError::InvalidPayload)?;
        Ok(Self {
            model_index: entity.model,
            origin: entity.origin,
            destructible_index: payload.destructible_index,
        })
    }

    pub const fn model_index(self) -> usize {
        self.model_index as usize
    }

    pub const fn transform(self) -> BrushTransform {
        BrushTransform::translated(self.origin)
    }

    pub const fn destructible_index(self) -> usize {
        self.destructible_index as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushDestructibleSetError {
    CapacityExceeded {
        max: usize,
    },
    MissingPayload {
        entity: usize,
    },
    InvalidDestructible {
        entity: usize,
        error: BrushDestructibleError,
    },
    MissingModel {
        entity: usize,
        model: u16,
    },
}

pub struct BrushDestructibleSet<const MAX_DESTRUCTIBLES: usize> {
    count: usize,
    entity_indices: [u16; MAX_DESTRUCTIBLES],
    destructibles: [BrushDestructible; MAX_DESTRUCTIBLES],
}

impl<const MAX_DESTRUCTIBLES: usize> BrushDestructibleSet<MAX_DESTRUCTIBLES> {
    pub const EMPTY: Self = Self {
        count: 0,
        entity_indices: [0; MAX_DESTRUCTIBLES],
        destructibles: [BrushDestructible::EMPTY; MAX_DESTRUCTIBLES],
    };

    pub fn init_from_map(
        &mut self,
        map: &PxbspResidentMap,
    ) -> Result<(), BrushDestructibleSetError> {
        self.init_from_entities(map.entities(), map.brush_models().len())
    }

    pub fn init_from_entities(
        &mut self,
        entities: PxbspEntityTable<'_>,
        brush_model_count: usize,
    ) -> Result<(), BrushDestructibleSetError> {
        *self = Self::EMPTY;
        let result = (|| {
            for entity_index in 0..entities.len() {
                let entity = entities
                    .get(entity_index)
                    .expect("entity index is inside checked table");
                if entity.class_id != entity_class::BRUSH_DESTRUCTIBLE {
                    continue;
                }
                if self.count == MAX_DESTRUCTIBLES {
                    return Err(BrushDestructibleSetError::CapacityExceeded {
                        max: MAX_DESTRUCTIBLES,
                    });
                }
                let payload = entities
                    .payload_record::<PxbspBrushDestructible>(entity_index)
                    .ok_or(BrushDestructibleSetError::MissingPayload {
                        entity: entity_index,
                    })?;
                let destructible =
                    BrushDestructible::from_entity(entity, payload).map_err(|error| {
                        BrushDestructibleSetError::InvalidDestructible {
                            entity: entity_index,
                            error,
                        }
                    })?;
                if destructible.model_index() >= brush_model_count {
                    return Err(BrushDestructibleSetError::MissingModel {
                        entity: entity_index,
                        model: entity.model,
                    });
                }
                self.entity_indices[self.count] = u16::try_from(entity_index).unwrap_or(u16::MAX);
                self.destructibles[self.count] = destructible;
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

    pub fn get(&self, index: usize) -> Option<&BrushDestructible> {
        self.destructibles.get(..self.count)?.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut BrushDestructible> {
        self.destructibles.get_mut(..self.count)?.get_mut(index)
    }

    pub fn entity_index(&self, index: usize) -> Option<usize> {
        self.entity_indices
            .get(..self.count)?
            .get(index)
            .copied()
            .map(usize::from)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &BrushDestructible> {
        self.destructibles[..self.count].iter()
    }
}

impl<const MAX_DESTRUCTIBLES: usize> Default for BrushDestructibleSet<MAX_DESTRUCTIBLES> {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxbsp::PxbspBrushDestructible;

    fn entity(flags: u16) -> PxbspEntity {
        PxbspEntity {
            class_id: entity_class::BRUSH_DESTRUCTIBLE,
            flags,
            model: 1,
            origin: Vec3I32 {
                x: 10 * 4096,
                y: 20 * 4096,
                z: 30 * 4096,
            },
            ..PxbspEntity::default()
        }
    }

    #[test]
    fn payload_links_brush_target_to_shared_destructible_state() {
        let item = BrushDestructible::from_entity(entity(0), PxbspBrushDestructible::new(7))
            .expect("horizon destructible");
        assert_eq!(item.destructible_index(), 7);
    }
}
