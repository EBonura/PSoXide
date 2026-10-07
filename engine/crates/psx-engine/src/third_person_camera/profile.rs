//! Bounded, allocation-free composition transitions for the follow camera.
use super::{Angle, ThirdPersonCameraConfig, WorldProjection};

/// Camera composition in room-local world units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraProfile {
    /// Trailing boom length.
    pub distance: i32,
    /// Camera height above the player root.
    pub height: i32,
    /// Focus height above the player root.
    pub target_height: i32,
    /// Signed camera-right offset; zero centres the player.
    pub shoulder_offset: i32,
    /// Vertical field of view, clamped to 38-48 degrees.
    pub fov_y_degrees: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct ProfileBlend {
    values: [i32; 5],
    distance: i32,
    active: bool,
    changing: bool,
    inherited_fov: i32,
    inherited_lens: bool,
    base_projection: Option<WorldProjection>,
}

impl ProfileBlend {
    pub const fn new() -> Self {
        Self {
            values: [0; 5],
            distance: 0,
            active: false,
            changing: false,
            inherited_fov: 43 * 4096,
            inherited_lens: true,
            base_projection: None,
        }
    }

    fn goal(&self, config: ThirdPersonCameraConfig, locked: bool) -> [i32; 5] {
        let base = ThirdPersonCameraProfile {
            distance: config.distance,
            height: config.height,
            target_height: config.target_height,
            fov_y_degrees: config.fov_y_degrees,
            shoulder_offset: config.shoulder_offset,
        };
        let profile = config.composition_override.unwrap_or_else(|| {
            if locked {
                config.lock_profile.unwrap_or(base)
            } else {
                base
            }
        });
        [
            profile
                .distance
                .clamp(config.min_distance.min(65_535), 65_535)
                * 4096,
            profile.height.clamp(0, 65_535) * 4096,
            profile.target_height.clamp(0, 65_535) * 4096,
            if profile.fov_y_degrees == 0 {
                self.inherited_fov
            } else {
                i32::from(profile.fov_y_degrees.clamp(38, 48)) * 4096
            },
            profile.shoulder_offset.clamp(-32_767, 32_767) * 4096,
        ]
    }

    pub fn set_base_projection(&mut self, projection: WorldProjection) {
        if self.base_projection == Some(projection) {
            return;
        }
        let mut low = 1u16;
        let mut high = 1023u16;
        // Invert the integer lens once per projection change, not every tick.
        while low < high {
            let middle = (low + high) / 2;
            let angle = Angle::from_q12(middle);
            if angle.sin().raw() * projection.focal_length.clamp(1, 65_535)
                < angle.cos().raw() * i32::from(projection.screen_y).max(1)
            {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let inherited = i32::from(low) * 720;
        if self.inherited_lens && self.values[3] == self.inherited_fov {
            self.values[3] = inherited;
        }
        self.inherited_fov = inherited;
        self.base_projection = Some(projection);
    }

    pub fn snap(&mut self, config: ThirdPersonCameraConfig, locked: bool) {
        self.active = config.blend_profiles
            || config.fov_y_degrees != 0
            || config.lock_profile.is_some()
            || config.composition_override.is_some();
        self.inherited_lens = if let Some(profile) = config.composition_override {
            profile.fov_y_degrees == 0
        } else if locked {
            config
                .lock_profile
                .map_or(config.fov_y_degrees == 0, |p| p.fov_y_degrees == 0)
        } else {
            config.fov_y_degrees == 0
        };
        self.values = self.goal(config, locked);
        self.distance = self.values[0];
        self.changing = false;
    }

    pub fn advance(&mut self, config: ThirdPersonCameraConfig, locked: bool) {
        let enabled = config.blend_profiles
            || config.fov_y_degrees != 0
            || config.lock_profile.is_some()
            || config.composition_override.is_some();
        if !self.active || !enabled || !config.blend_profiles {
            let before = self.values;
            self.snap(config, locked);
            self.changing = enabled && before != self.values;
            return;
        }
        self.inherited_lens = if let Some(profile) = config.composition_override {
            profile.fov_y_degrees == 0
        } else if locked {
            config
                .lock_profile
                .map_or(config.fov_y_degrees == 0, |p| p.fov_y_degrees == 0)
        } else {
            config.fov_y_degrees == 0
        };
        let goal = self.goal(config, locked);
        let before = self.values;
        let distance_before = self.distance;
        // 1-sqrt(1-alpha30), rounded to Q12: .05 -> 104, .1 -> 210.
        let response = i32::from(config.profile_response_q12.clamp(1, 4096));
        for (value, target) in self.values.iter_mut().zip(goal) {
            *value = approach(*value, target, response);
        }
        self.distance = approach(self.distance, self.values[0], response.max(210));
        self.changing = before != self.values || distance_before != self.distance;
    }

    pub fn apply(&self, mut config: ThirdPersonCameraConfig) -> ThirdPersonCameraConfig {
        if self.active {
            config.distance = ((self.distance + 2048) / 4096).max(config.min_distance);
            config.max_distance = config.max_distance.max(config.distance);
            config.height = (self.values[1] + 2048) / 4096;
            config.target_height = (self.values[2] + 2048) / 4096;
            config.fov_y_degrees = ((self.values[3] + 2048) / 4096) as u8;
            config.shoulder_offset = self.values[4] / 4096;
        }
        config
    }

    pub fn projection(&self, mut base: WorldProjection) -> WorldProjection {
        if self.active && !(self.inherited_lens && self.values[3] == self.inherited_fov) {
            let half = Angle::from_q12((self.values[3] / 720) as u16);
            base.focal_length = (i32::from(base.screen_y).max(1) * half.cos().raw()
                / half.sin().raw().max(1))
            .max(1);
        }
        base
    }

    pub fn vertical_fov_degrees(&self) -> u8 {
        let value = if self.active {
            self.values[3]
        } else {
            self.inherited_fov
        };
        ((value + 2048) / 4096).clamp(1, 179) as u8
    }

    pub const fn changing(&self) -> bool {
        self.changing
    }
}

fn approach(value: i32, goal: i32, rate: i32) -> i32 {
    let error = goal - value;
    // Split before multiplication to stay in 32-bit arithmetic for large worlds.
    let step = (error / 4096) * rate + (error % 4096) * rate / 4096;
    value + if step == 0 { error.signum() } else { step }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn held_composition_is_independent_of_lock_and_reverses_without_reset() {
        let mut c = config();
        c.profile_response_q12 = 1024;
        let aim = ThirdPersonCameraProfile {
            distance: 160,
            height: 96,
            target_height: 80,
            shoulder_offset: 24,
            fov_y_degrees: 43,
        };
        for locked in [false, true] {
            let mut blend = ProfileBlend::new();
            blend.snap(c, locked);
            let normal = blend.apply(c);
            c.composition_override = Some(aim);
            for _ in 0..6 {
                blend.advance(c, locked);
            }
            let partial = blend.apply(c);
            assert!(partial.distance > aim.distance && partial.distance < normal.distance);
            assert!(partial.shoulder_offset > 0 && partial.shoulder_offset < 24);
            c.composition_override = None;
            blend.advance(c, locked);
            assert!(blend.apply(c).shoulder_offset < partial.shoulder_offset);
            assert!(blend.apply(c).shoulder_offset > 0);
            c.composition_override = Some(aim);
            for _ in 0..24 {
                blend.advance(c, !locked);
            }
            assert!((blend.apply(c).distance - aim.distance).abs() <= 1);
            assert!((blend.apply(c).shoulder_offset - 24).abs() <= 1);
            c.composition_override = None;
            for _ in 0..120 {
                blend.advance(c, locked);
            }
            assert_eq!(blend.apply(c).distance, normal.distance);
            assert_eq!(blend.apply(c).shoulder_offset, 0);
        }
    }
    fn config() -> ThirdPersonCameraConfig {
        let mut c = ThirdPersonCameraConfig::character(219, 113, 73);
        c.fov_y_degrees = 43;
        c.blend_profiles = true;
        c.lock_profile = Some(ThirdPersonCameraProfile {
            distance: 244,
            height: 119,
            target_height: 78,
            fov_y_degrees: 46,
            shoulder_offset: 0,
        });
        c
    }
    #[test]
    fn profiles_blend_all_fields_and_distance_has_a_second_stage() {
        let c = config();
        let mut blend = ProfileBlend::new();
        let projection = WorldProjection::new(160, 120, 320, 4);
        blend.set_base_projection(projection);
        blend.snap(c, false);
        assert!((303..=306).contains(&blend.projection(projection).focal_length));
        blend.advance(c, true);
        assert!(blend.values[0] > 219 * 4096 && blend.values[0] < 244 * 4096);
        assert!(blend.distance < blend.values[0]);
        for _ in 0..1000 {
            blend.advance(c, true);
        }
        assert_eq!(
            (
                blend.apply(c).distance,
                blend.apply(c).height,
                blend.apply(c).target_height
            ),
            (244, 119, 78)
        );
        assert!((281..=284).contains(&blend.projection(projection).focal_length));
        for _ in 0..1000 {
            blend.advance(c, false);
        }
        assert_eq!(
            (
                blend.apply(c).distance,
                blend.apply(c).height,
                blend.apply(c).target_height
            ),
            (219, 113, 73)
        );
        assert!(!blend.changing());
    }
    #[test]
    fn profile_unlock_restores_an_inherited_lens_exactly() {
        let mut c = config();
        c.fov_y_degrees = 0;
        let projection = WorldProjection::new(160, 120, 320, 4);
        let mut blend = ProfileBlend::new();
        blend.set_base_projection(projection);
        blend.snap(c, false);
        assert_eq!(blend.projection(projection), projection);
        for _ in 0..1000 {
            blend.advance(c, true);
        }
        assert!(blend.projection(projection).focal_length < projection.focal_length);
        for _ in 0..1000 {
            blend.advance(c, false);
        }
        assert_eq!(blend.projection(projection), projection);
        c.lock_profile = None;
        c.blend_profiles = false;
        blend.advance(c, false);
        assert_eq!(blend.apply(c), c);
        assert_eq!(blend.projection(projection), projection);
    }
    #[test]
    fn enabling_a_lens_override_on_a_blended_camera_takes_effect_and_can_be_cleared() {
        let mut c = ThirdPersonCameraConfig::character(219, 113, 73);
        c.blend_profiles = true;
        let projection = WorldProjection::new(160, 120, 320, 4);
        let mut blend = ProfileBlend::new();
        blend.set_base_projection(projection);
        blend.snap(c, false);
        assert_eq!(blend.projection(projection), projection);
        c.fov_y_degrees = 48;
        for _ in 0..1000 {
            blend.advance(c, false);
        }
        assert!(blend.projection(projection).focal_length < 280);
        c.fov_y_degrees = 0;
        blend.advance(c, false);
        assert_ne!(blend.projection(projection), projection);
        for _ in 0..1000 {
            blend.advance(c, false);
        }
        assert_eq!(blend.projection(projection), projection);
    }

    #[test]
    fn profile_lens_changes_invalidate_collision_even_without_blending() {
        let mut c = config();
        c.blend_profiles = false;
        let mut blend = ProfileBlend::new();
        blend.snap(c, false);
        blend.advance(c, true);
        assert!(blend.changing());
        assert_eq!(blend.apply(c).distance, 244);
        blend.advance(c, true);
        assert!(!blend.changing());
    }
}
