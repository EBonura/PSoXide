//! The world's global light settings: ambient occlusion, sky light and
//! bounced light, baked into the corner colours.

use serde::{Deserialize, Serialize};

/// How the bake gathers light that did not come straight from a lamp.
///
/// With it off the corner colour is the room ambient plus the lamps, as every
/// world made before the setting has it. With it on, each corner samples the
/// hemisphere above it: a ray that meets a wall nearby darkens the ambient
/// (occlusion), a ray that escapes the world brings in the sky colour, and a
/// ray that lands on a lit wall brings in that wall's colour, scaled by its
/// texture's average colour (bounce). All of it is computed on the host and
/// baked into the vertex colours, so the console pays nothing.
///
/// The fields are whole numbers so the world stays comparable and stable on
/// disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalLight {
    /// Whether the gather runs at all. A world saved before the setting
    /// loads with it off, so levels never change behind their author's back.
    pub enabled: bool,
    /// Percent of the ambient a blocked ray takes away, 0 to 100. At 0 the
    /// ambient stays flat; at 100 a corner boxed in by walls gets only what
    /// the walls bounce back.
    pub occlusion: u8,
    /// How far an occluder counts, in sectors, 1 to 16. A wall further off
    /// than this no longer darkens the corner.
    pub reach: u8,
    /// Passes of bounced light after the first gather, 0 to 3.
    pub bounces: u8,
    /// Percent of the light a surface passes on when it bounces, 0 to 200;
    /// 100 is the texture's average colour as it is. A new world starts at
    /// 150: a PS1 surface shows its texture times a light value that can
    /// reach twice neutral, so a lit wall hands on more than its plain
    /// average, and with 150 the gathered levels keep close to the average
    /// brightness of the flat ambient they replace.
    pub bounce_gain: u8,
    /// Rays per corner, 8 to 128. More is smoother and slower to bake.
    pub rays: u8,
    /// Colour a ray that escapes the world brings in; `None` uses the room's
    /// ambient, so an open sky looks like an ambient room until you pick one.
    pub sky: Option<[u8; 3]>,
}

impl GlobalLight {
    /// The values a new world starts from.
    pub const DEFAULT: Self = Self {
        enabled: false,
        occlusion: 60,
        reach: 4,
        bounces: 2,
        bounce_gain: 150,
        rays: 64,
        sky: None,
    };

    /// What a new world gets: the gather on with the default values.
    pub const NEW_WORLD: Self = Self {
        enabled: true,
        ..Self::DEFAULT
    };

    /// Whether the document can leave the setting out (the default, off).
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// The values clamped to what the bake accepts.
    pub fn clamped(self) -> Self {
        Self {
            occlusion: self.occlusion.min(100),
            reach: self.reach.clamp(1, 16),
            bounces: self.bounces.min(3),
            bounce_gain: self.bounce_gain.min(200),
            rays: self.rays.clamp(8, 128),
            ..self
        }
    }
}

impl Default for GlobalLight {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_world_saved_without_the_setting_loads_with_it_off() {
        let loaded: GlobalLight = ron::from_str("()").unwrap();
        assert!(!loaded.enabled);
        assert!(loaded.is_default());
    }

    #[test]
    fn the_setting_round_trips_and_the_default_is_left_out() {
        let on = GlobalLight::NEW_WORLD;
        let text = ron::to_string(&on).unwrap();
        assert_eq!(ron::from_str::<GlobalLight>(&text).unwrap(), on);
        let custom = GlobalLight {
            sky: Some([90, 120, 200]),
            rays: 64,
            ..on
        };
        let text = ron::to_string(&custom).unwrap();
        assert_eq!(ron::from_str::<GlobalLight>(&text).unwrap(), custom);
        assert!(GlobalLight::default().is_default());
        assert!(!on.is_default());
    }
}
