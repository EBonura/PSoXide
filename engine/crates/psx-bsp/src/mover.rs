//! Fixed-tick motion state for PXBSP brush submodels.

use crate::collision::BrushTransform;
use crate::pxbsp::{entity_class, entity_flags, PxbspBrushDoor, PxbspBrushDoorError, PxbspEntity};
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

fn endpoint_axis(origin: i32, offset: i32, progress: u16, duration: u16) -> i32 {
    let delta = i64::from(offset) * i64::from(progress) / i64::from(duration);
    origin.saturating_add(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
