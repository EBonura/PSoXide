//! Parry feedback: the two tells that make a parry readable, built once
//! for whichever fighter parries.
//!
//! * A **flash** on the parrying fighter's model: the model's lit tint is
//!   pulled toward the parrier's stance colour and fades over a few ticks
//!   ([`ParryFlash`]). It rides the existing per-draw lit tint
//!   ([`LitTintBias`], one lerp per draw), so no packet is touched and a
//!   fighter that is not flashing draws exactly as before.
//! * A **burst** at the contact point: a ring with a star of spokes in the
//!   stance colour, drawn by the impact renderer from a
//!   [`ProjectileVisualStyle`] whose `star_points` is non-zero
//!   ([`ParryFeedback::burst_style`]). It uses the existing impact pool and
//!   particle texture; no texture is generated for it.
//!
//! Nothing here is gameplay: no fighter freezes, and no gameplay state reads
//! these values. Every duration, size and colour is the data in
//! [`ParryFeedback`].

use crate::model_rendering::LitTintBias;
use crate::projectiles::ProjectileVisualStyle;

/// The data of both parry tells. One global value today
/// ([`PARRY_FEEDBACK`]); every field is a placeholder that the combat
/// harness tunes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParryFeedback {
    /// How long the model flash lasts, in 60 Hz simulation ticks (at most
    /// 127).
    pub flash_ticks: u8,
    /// How far the lit tint moves toward the stance colour on the first tick
    /// of the flash, Q8 (256 replaces the lit tint). It fades linearly to
    /// zero over `flash_ticks`.
    pub flash_peak_q8: u16,
    /// How long the burst lasts, in 60 Hz simulation ticks.
    pub burst_ticks: u8,
    /// World radius the burst is sized from, engine units.
    pub burst_radius: u16,
    /// How far the burst's ring and spokes reach, relative to its on-screen
    /// size (`256 = 1x`).
    pub burst_reach_q8: u16,
    /// Number of spokes in the star (alternately long and short).
    pub burst_points: u8,
    /// Flash and burst colour of the Horizon stance. The HUD's Horizon
    /// orange.
    pub horizon_rgb: [u8; 3],
    /// Flash and burst colour of the Zenith stance. The HUD's Zenith teal.
    pub zenith_rgb: [u8; 3],
    /// Near-white hot core of the burst and its long spokes.
    pub burst_core_rgb: [u8; 3],
}

/// The parry feedback data in use.
// placeholder, harness tunes
pub const PARRY_FEEDBACK: ParryFeedback = ParryFeedback {
    flash_ticks: 12,
    flash_peak_q8: 256,
    burst_ticks: 20,
    burst_radius: 48,
    burst_reach_q8: 640,
    burst_points: 8,
    horizon_rgb: [255, 113, 58],
    zenith_rgb: [108, 224, 198],
    burst_core_rgb: [255, 244, 224],
};

impl ParryFeedback {
    /// The stance colour of the fighter that parried.
    pub const fn stance_rgb(&self, zenith: bool) -> [u8; 3] {
        if zenith {
            self.zenith_rgb
        } else {
            self.horizon_rgb
        }
    }

    /// The burst's presentation in the stance colour. Spawn it into the
    /// impact pool at the contact point with [`Self::burst_radius`].
    pub const fn burst_style(&self, zenith: bool) -> ProjectileVisualStyle {
        let accent = self.stance_rgb(zenith);
        ProjectileVisualStyle {
            core_rgb: self.burst_core_rgb,
            glow_rgb: accent,
            impact_rgb: accent,
            glow_scale_q8: self.burst_reach_q8,
            impact_lifetime_ticks: self.burst_ticks,
            star_points: self.burst_points,
            ..ProjectileVisualStyle::EMPTY
        }
    }
}

/// One fighter's model flash: the ticks left and the stance colour it was
/// started in (a later swap does not recolor it). One byte per fighter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParryFlash(u8);

impl ParryFlash {
    /// No flash.
    pub const NONE: Self = Self(0);
    const ZENITH: u8 = 0x80;
    const TICKS: u8 = 0x7f;

    /// A flash in the parrier's stance colour, for `data.flash_ticks` ticks.
    pub const fn start(data: &ParryFeedback, zenith: bool) -> Self {
        let ticks = if data.flash_ticks > Self::TICKS {
            Self::TICKS
        } else {
            data.flash_ticks
        };
        if ticks == 0 {
            Self::NONE
        } else if zenith {
            Self(ticks | Self::ZENITH)
        } else {
            Self(ticks)
        }
    }

    /// Whether the flash is showing.
    pub const fn is_active(self) -> bool {
        self.0 & Self::TICKS != 0
    }

    /// Whether the flash was started in the Zenith stance.
    pub const fn is_zenith(self) -> bool {
        self.0 & Self::ZENITH != 0
    }

    /// Ticks left.
    pub const fn ticks_left(self) -> u8 {
        self.0 & Self::TICKS
    }

    /// Advance one 60 Hz simulation tick.
    pub fn tick(&mut self) {
        if self.is_active() {
            self.0 -= 1;
        }
    }

    /// The lit tint bias for this tick, over the fighter's `base` bias (the
    /// player's stance tint; `None` for a fighter with none). The flash's
    /// weight is its peak strength on the first tick, fading linearly to
    /// zero: the stance colour at that weight over `base`. A fighter with no
    /// flash gets `base` back unchanged, so it draws exactly as it always
    /// did, and a finished flash lands on `base` with no jump.
    pub fn lit_tint(self, data: &ParryFeedback, base: Option<LitTintBias>) -> Option<LitTintBias> {
        if !self.is_active() || data.flash_ticks == 0 {
            return base;
        }
        let left = u32::from(self.ticks_left());
        let weight = (u32::from(data.flash_peak_q8) * left / u32::from(data.flash_ticks)).min(256);
        let [r, g, b] = data.stance_rgb(self.is_zenith());
        let Some(base) = base else {
            return Some(LitTintBias {
                color: (r, g, b),
                strength_q8: weight as u16,
            });
        };
        let lerp = |from: u8, to: u8| {
            let (from, to, weight) = (i32::from(from), i32::from(to), weight as i32);
            (from + (to - from) * weight / 256).clamp(0, 255) as u8
        };
        let strength = i32::from(base.strength_q8.min(256));
        Some(LitTintBias {
            color: (
                lerp(base.color.0, r),
                lerp(base.color.1, g),
                lerp(base.color.2, b),
            ),
            strength_q8: (strength + (256 - strength) * weight as i32 / 256) as u16,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flash_fades_to_nothing_in_its_stance_colour() {
        let data = PARRY_FEEDBACK;
        let mut flash = ParryFlash::start(&data, true);
        let first = flash.lit_tint(&data, None).unwrap();
        assert_eq!(first.color, (108, 224, 198));
        assert_eq!(first.strength_q8, data.flash_peak_q8.min(256));
        let mut last = first.strength_q8;
        for _ in 1..data.flash_ticks {
            flash.tick();
            let strength = flash.lit_tint(&data, None).unwrap().strength_q8;
            assert!(strength < last, "strictly fading");
            last = strength;
        }
        flash.tick();
        assert!(!flash.is_active());
        assert_eq!(flash.lit_tint(&data, None), None);
        assert_eq!(
            ParryFlash::start(&data, false)
                .lit_tint(&data, None)
                .unwrap()
                .color,
            (255, 113, 58)
        );
    }

    #[test]
    fn over_a_base_tint_the_flash_starts_at_the_stance_colour_and_returns_to_the_base() {
        let data = PARRY_FEEDBACK;
        let base = LitTintBias {
            color: (214, 75, 48),
            strength_q8: 208,
        };
        assert_eq!(ParryFlash::NONE.lit_tint(&data, Some(base)), Some(base));
        assert_eq!(ParryFlash::NONE.lit_tint(&data, None), None);
        let mut flash = ParryFlash::start(&data, false);
        let first = flash.lit_tint(&data, Some(base)).unwrap();
        assert_eq!(first.color, (255, 113, 58));
        assert_eq!(first.strength_q8, 256);
        // The last tick is close to the base, and there is no jump after it.
        for _ in 1..data.flash_ticks {
            flash.tick();
        }
        let last = flash.lit_tint(&data, Some(base)).unwrap();
        assert!(last.strength_q8 >= base.strength_q8 && last.strength_q8 < 256);
        assert!(last.color.0.abs_diff(base.color.0) <= 4);
        flash.tick();
        assert_eq!(flash.lit_tint(&data, Some(base)), Some(base));
    }

    #[test]
    fn zero_ticks_turn_the_flash_off_and_the_burst_carries_its_data() {
        let mut data = PARRY_FEEDBACK;
        data.flash_ticks = 0;
        assert_eq!(ParryFlash::start(&data, true), ParryFlash::NONE);
        let style = PARRY_FEEDBACK.burst_style(false);
        assert_eq!(style.impact_rgb, PARRY_FEEDBACK.horizon_rgb);
        assert_eq!(style.star_points, PARRY_FEEDBACK.burst_points);
        assert_eq!(style.impact_lifetime_ticks, PARRY_FEEDBACK.burst_ticks);
        assert_eq!(
            PARRY_FEEDBACK.burst_style(true).impact_rgb,
            PARRY_FEEDBACK.zenith_rgb
        );
    }
}
