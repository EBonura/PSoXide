//! Combat feedback: the tells that make a landed hit or a parry readable,
//! built once for whichever fighter is on the receiving end.
//!
//! * A **flash** on the struck fighter's model ([`CombatFlash`], one byte per
//!   fighter): the model's lit tint is pulled toward a colour and fades over a
//!   few ticks. It rides the existing per-draw lit tint ([`LitTintBias`], one
//!   lerp per draw), so no packet is touched and a fighter that is not
//!   flashing draws exactly as before. A parry flashes the parrier in its
//!   stance colour; a hit flashes the victim white-hot, cooling toward the
//!   attacker's stance colour, stronger and longer for a heavy hit and a crit
//!   ([`FlashKind`]). A parry flash is never replaced by a hit.
//! * A **burst** at the contact point, drawn by the impact renderer from a
//!   [`ProjectileVisualStyle`] whose `star_points` is non-zero. The parry's is
//!   a ring with spokes in the parrier's stance colour
//!   ([`CombatFeedback::burst_style`]); a melee hit's is a small ringless
//!   spray of spokes ([`CombatFeedback::spark_style`]). Both use the existing
//!   impact pool and particle texture; no texture is generated for them.
//!
//! Nothing here is gameplay: no fighter freezes, and no gameplay state reads
//! these values. Every duration, size and colour is the data in
//! [`CombatFeedback`].

use crate::combatant::CombatantId;
use crate::model_rendering::LitTintBias;
use crate::projectiles::ProjectileVisualStyle;

/// What a flash tells, weakest first. A stronger flash replaces a weaker
/// one; nothing replaces a [`FlashKind::Parry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlashKind {
    /// An ordinary landed hit.
    Hit = 0,
    /// A hit that broke posture, or a heavy swing.
    Heavy = 1,
    /// A crit.
    Crit = 2,
    /// The parrier's own flash.
    Parry = 3,
}

impl FlashKind {
    /// The kind of an ordinary hit: a crit, else a heavy one (a heavy swing
    /// or a posture break), else a light one.
    pub const fn of_hit(crit: bool, heavy: bool) -> Self {
        if crit {
            Self::Crit
        } else if heavy {
            Self::Heavy
        } else {
            Self::Hit
        }
    }
}

/// One landed hit, for whoever shows it ([`COMBAT_FEEDBACK`] says how).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LandedHit {
    /// The fighter that was struck.
    pub victim: CombatantId,
    /// Where the attacker stands (feet) for a melee contact, which also gets
    /// a spark; `None` for a bolt, which has its own impact.
    pub contact_from: Option<[i32; 3]>,
    /// How hard it read: light, heavy or crit.
    pub kind: FlashKind,
    /// The attacker's stance colour is Zenith.
    pub zenith: bool,
}

/// How long one kind of flash lasts and how hard it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashLook {
    /// Duration in 60 Hz simulation ticks (at most 31).
    pub ticks: u8,
    /// How far the lit tint moves toward the flash colour on the first tick,
    /// Q8 (256 replaces the lit tint). It fades linearly to zero.
    pub peak_q8: u16,
}

/// The data of every combat tell. One global value today
/// ([`COMBAT_FEEDBACK`]); every field is a placeholder that the combat
/// harness tunes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatFeedback {
    /// The model flash of each [`FlashKind`], indexed by it.
    pub flash: [FlashLook; 4],
    /// How long the parry burst lasts, in 60 Hz simulation ticks.
    pub burst_ticks: u8,
    /// World radius the parry burst is sized from, engine units.
    pub burst_radius: u16,
    /// How far the parry burst's ring and spokes reach, relative to its
    /// on-screen size (`256 = 1x`).
    pub burst_reach_q8: u16,
    /// Number of spokes in the parry star (alternately long and short).
    pub burst_points: u8,
    /// How long a melee hit's spark lasts, in 60 Hz simulation ticks.
    pub spark_ticks: u8,
    /// World radius a hit spark is sized from, engine units.
    pub spark_radius: u16,
    /// How far a hit spark's spokes reach, relative to its on-screen size
    /// (`256 = 1x`), for [`FlashKind::Hit`], [`FlashKind::Heavy`] and
    /// [`FlashKind::Crit`].
    pub spark_reach_q8: [u16; 3],
    /// Number of spokes in a hit spark.
    pub spark_points: u8,
    /// Flash and burst colour of the Horizon stance. The HUD's Horizon
    /// orange.
    pub horizon_rgb: [u8; 3],
    /// Flash and burst colour of the Zenith stance. The HUD's Zenith teal.
    pub zenith_rgb: [u8; 3],
    /// Near-white hot core of the bursts and sparks, and the colour a hit
    /// flash starts in.
    pub burst_core_rgb: [u8; 3],
}

/// The combat feedback data in use.
// placeholder, harness tunes
pub const COMBAT_FEEDBACK: CombatFeedback = CombatFeedback {
    flash: [
        FlashLook {
            ticks: 6,
            peak_q8: 224,
        },
        FlashLook {
            ticks: 8,
            peak_q8: 256,
        },
        FlashLook {
            ticks: 11,
            peak_q8: 256,
        },
        FlashLook {
            ticks: 12,
            peak_q8: 256,
        },
    ],
    burst_ticks: 20,
    burst_radius: 32,
    burst_reach_q8: 512,
    burst_points: 8,
    spark_ticks: 9,
    spark_radius: 14,
    spark_reach_q8: [256, 352, 448],
    spark_points: 6,
    horizon_rgb: [255, 113, 58],
    zenith_rgb: [108, 224, 198],
    burst_core_rgb: [255, 244, 224],
};

/// Set in [`ProjectileVisualStyle::star_points`] for a ringless spark; the
/// other bits are the spoke count.
pub const STAR_NO_RING: u8 = 0x80;

impl CombatFeedback {
    /// The stance colour of the fighter that parried (or, for a hit, of the
    /// fighter that struck).
    pub const fn stance_rgb(&self, zenith: bool) -> [u8; 3] {
        if zenith {
            self.zenith_rgb
        } else {
            self.horizon_rgb
        }
    }

    /// The parry burst's presentation in the stance colour. Spawn it into the
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

    /// A melee hit's spark in the attacker's stance colour, sized by the
    /// hit's `kind` (a [`FlashKind::Parry`] takes the crit size). Spawn it
    /// into the impact pool at the contact point with [`Self::spark_radius`].
    pub const fn spark_style(&self, kind: FlashKind, zenith: bool) -> ProjectileVisualStyle {
        let accent = self.stance_rgb(zenith);
        let reach = match kind {
            FlashKind::Hit => self.spark_reach_q8[0],
            FlashKind::Heavy => self.spark_reach_q8[1],
            _ => self.spark_reach_q8[2],
        };
        ProjectileVisualStyle {
            core_rgb: self.burst_core_rgb,
            glow_rgb: accent,
            impact_rgb: accent,
            glow_scale_q8: reach,
            impact_lifetime_ticks: self.spark_ticks,
            star_points: self.spark_points | STAR_NO_RING,
            ..ProjectileVisualStyle::EMPTY
        }
    }
}

/// One fighter's model flash: the ticks left, the kind and the stance colour
/// it was started in (a later swap does not recolor it). One byte per
/// fighter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatFlash(u8);

impl CombatFlash {
    /// No flash.
    pub const NONE: Self = Self(0);
    const ZENITH: u8 = 0x80;
    const KIND_SHIFT: u8 = 5;
    const KIND: u8 = 0x03 << Self::KIND_SHIFT;
    const TICKS: u8 = 0x1f;

    /// A flash of `kind` in the stance colour of `zenith`, for the kind's
    /// duration in `data`.
    pub const fn start(data: &CombatFeedback, kind: FlashKind, zenith: bool) -> Self {
        let ticks = data.flash[kind as usize].ticks;
        let ticks = if ticks > Self::TICKS {
            Self::TICKS
        } else {
            ticks
        };
        if ticks == 0 {
            return Self::NONE;
        }
        let zenith = if zenith { Self::ZENITH } else { 0 };
        Self(ticks | ((kind as u8) << Self::KIND_SHIFT) | zenith)
    }

    /// Start a flash of `kind` unless a stronger one is still showing: a
    /// parry is never replaced by a hit, and a crit not by a lighter hit.
    pub fn begin(&mut self, data: &CombatFeedback, kind: FlashKind, zenith: bool) {
        if !self.is_active() || kind >= self.kind() {
            *self = Self::start(data, kind, zenith);
        }
    }

    /// Whether the flash is showing.
    pub const fn is_active(self) -> bool {
        self.0 & Self::TICKS != 0
    }

    /// Whether the flash is in the Zenith stance colour.
    pub const fn is_zenith(self) -> bool {
        self.0 & Self::ZENITH != 0
    }

    /// What the flash tells. A fighter with no flash reads as a
    /// [`FlashKind::Hit`].
    pub const fn kind(self) -> FlashKind {
        match (self.0 & Self::KIND) >> Self::KIND_SHIFT {
            0 => FlashKind::Hit,
            1 => FlashKind::Heavy,
            2 => FlashKind::Crit,
            _ => FlashKind::Parry,
        }
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

    /// The flash's weight this tick, Q8: the kind's peak strength on the
    /// first tick, fading linearly to zero (0 when there is no flash). The
    /// model draws read it as a lit tint ([`Self::lit_tint`]) or, for a
    /// player mid stance swap, through the phase assembly's own flash.
    pub fn weight_q8(self, data: &CombatFeedback) -> u16 {
        let look = data.flash[self.kind() as usize];
        if !self.is_active() || look.ticks == 0 {
            return 0;
        }
        let left = u32::from(self.ticks_left());
        (u32::from(look.peak_q8) * left / u32::from(look.ticks)).min(256) as u16
    }

    /// The flash colour this tick. A parry is the stance colour throughout.
    /// A hit starts in the hot core colour and cools into the stance colour
    /// as it fades.
    fn rgb(self, data: &CombatFeedback) -> [u8; 3] {
        let stance = data.stance_rgb(self.is_zenith());
        let ticks = data.flash[self.kind() as usize].ticks;
        if self.kind() == FlashKind::Parry || ticks == 0 {
            return stance;
        }
        let hot = i32::from(self.ticks_left()) * 256 / i32::from(ticks);
        let mix = |cool: u8, hot_value: u8| {
            let (cool, hot_value) = (i32::from(cool), i32::from(hot_value));
            (cool + (hot_value - cool) * hot / 256).clamp(0, 255) as u8
        };
        [
            mix(stance[0], data.burst_core_rgb[0]),
            mix(stance[1], data.burst_core_rgb[1]),
            mix(stance[2], data.burst_core_rgb[2]),
        ]
    }

    /// The lit tint bias for this tick, over the fighter's `base` bias (the
    /// player's stance tint; `None` for a fighter with none). The flash's
    /// weight is its peak strength on the first tick, fading linearly to
    /// zero: the flash colour at that weight over `base`. A fighter with no
    /// flash gets `base` back unchanged, so it draws exactly as it always
    /// did, and a finished flash lands on `base` with no jump.
    pub fn lit_tint(self, data: &CombatFeedback, base: Option<LitTintBias>) -> Option<LitTintBias> {
        if !self.is_active() {
            return base;
        }
        let weight = u32::from(self.weight_q8(data));
        let [r, g, b] = self.rgb(data);
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

    const PARRY: FlashKind = FlashKind::Parry;

    fn parry_ticks() -> u8 {
        COMBAT_FEEDBACK.flash[PARRY as usize].ticks
    }

    #[test]
    fn a_parry_flash_fades_to_nothing_in_its_stance_colour() {
        let data = COMBAT_FEEDBACK;
        let mut flash = CombatFlash::start(&data, PARRY, true);
        let first = flash.lit_tint(&data, None).unwrap();
        assert_eq!(first.color, (108, 224, 198));
        assert_eq!(
            first.strength_q8,
            data.flash[PARRY as usize].peak_q8.min(256)
        );
        let mut last = first.strength_q8;
        for _ in 1..parry_ticks() {
            flash.tick();
            let tint = flash.lit_tint(&data, None).unwrap();
            assert!(tint.strength_q8 < last, "strictly fading");
            assert_eq!(tint.color, (108, 224, 198), "a parry keeps its colour");
            last = tint.strength_q8;
        }
        flash.tick();
        assert!(!flash.is_active());
        assert_eq!(flash.lit_tint(&data, None), None);
        assert_eq!(
            CombatFlash::start(&data, PARRY, false)
                .lit_tint(&data, None)
                .unwrap()
                .color,
            (255, 113, 58)
        );
    }

    #[test]
    fn over_a_base_tint_the_flash_starts_at_the_flash_colour_and_returns_to_the_base() {
        let data = COMBAT_FEEDBACK;
        let base = LitTintBias {
            color: (214, 75, 48),
            strength_q8: 208,
        };
        assert_eq!(CombatFlash::NONE.lit_tint(&data, Some(base)), Some(base));
        assert_eq!(CombatFlash::NONE.lit_tint(&data, None), None);
        let mut flash = CombatFlash::start(&data, PARRY, false);
        let first = flash.lit_tint(&data, Some(base)).unwrap();
        assert_eq!(first.color, (255, 113, 58));
        assert_eq!(first.strength_q8, 256);
        // The last tick is close to the base, and there is no jump after it.
        for _ in 1..parry_ticks() {
            flash.tick();
        }
        let last = flash.lit_tint(&data, Some(base)).unwrap();
        assert!(last.strength_q8 >= base.strength_q8 && last.strength_q8 < 256);
        assert!(last.color.0.abs_diff(base.color.0) <= 4);
        flash.tick();
        assert_eq!(flash.lit_tint(&data, Some(base)), Some(base));
    }

    #[test]
    fn a_hit_flash_starts_white_hot_and_cools_into_the_attackers_stance_colour() {
        let data = COMBAT_FEEDBACK;
        let mut flash = CombatFlash::start(&data, FlashKind::Hit, true);
        let first = flash.lit_tint(&data, None).unwrap();
        // First tick: nearly the hot core colour, not the teal.
        assert!(first.color.0 > 200 && first.color.1 > 230, "{first:?}");
        let ticks = data.flash[FlashKind::Hit as usize].ticks;
        for _ in 1..ticks {
            flash.tick();
        }
        let last = flash.lit_tint(&data, None).unwrap();
        assert!(last.color.0 < first.color.0, "cooled toward teal red");
        assert!(last.color.0.abs_diff(108) < 60 && last.color.1.abs_diff(224) < 40);
        assert!(last.strength_q8 < first.strength_q8);
    }

    #[test]
    fn heavier_hits_and_crits_flash_harder_and_longer() {
        let data = COMBAT_FEEDBACK;
        let look = |kind: FlashKind| (CombatFlash::start(&data, kind, false), kind);
        let strength = |kind: FlashKind| look(kind).0.weight_q8(&data);
        assert!(strength(FlashKind::Hit) < strength(FlashKind::Heavy));
        assert!(strength(FlashKind::Heavy) <= strength(FlashKind::Crit));
        let ticks = |kind: FlashKind| look(kind).0.ticks_left();
        assert!(ticks(FlashKind::Hit) < ticks(FlashKind::Heavy));
        assert!(ticks(FlashKind::Heavy) < ticks(FlashKind::Crit));
        for kind in [
            FlashKind::Hit,
            FlashKind::Heavy,
            FlashKind::Crit,
            FlashKind::Parry,
        ] {
            assert_eq!(look(kind).0.kind(), kind);
        }
    }

    #[test]
    fn a_crit_outranks_a_heavy_hit_which_outranks_a_plain_one() {
        assert_eq!(FlashKind::of_hit(false, false), FlashKind::Hit);
        assert_eq!(FlashKind::of_hit(false, true), FlashKind::Heavy);
        assert_eq!(FlashKind::of_hit(true, false), FlashKind::Crit);
        assert_eq!(FlashKind::of_hit(true, true), FlashKind::Crit);
    }

    #[test]
    fn a_parry_flash_is_never_replaced_by_a_hit_and_a_lighter_hit_keeps_the_heavier() {
        let data = COMBAT_FEEDBACK;
        let mut flash = CombatFlash::NONE;
        flash.begin(&data, FlashKind::Crit, false);
        flash.begin(&data, FlashKind::Hit, true);
        assert_eq!(flash.kind(), FlashKind::Crit);
        assert!(!flash.is_zenith());
        flash.begin(&data, PARRY, true);
        assert_eq!(flash.kind(), PARRY);
        let parry = flash;
        for kind in [FlashKind::Hit, FlashKind::Heavy, FlashKind::Crit] {
            flash.begin(&data, kind, false);
            assert_eq!(flash, parry);
        }
        // Once it has finished a hit may flash again.
        for _ in 0..parry_ticks() {
            flash.tick();
        }
        flash.begin(&data, FlashKind::Hit, false);
        assert_eq!(flash.kind(), FlashKind::Hit);
    }

    #[test]
    fn zero_ticks_turn_a_flash_off_and_the_styles_carry_their_data() {
        let mut data = COMBAT_FEEDBACK;
        data.flash[PARRY as usize].ticks = 0;
        assert_eq!(CombatFlash::start(&data, PARRY, true), CombatFlash::NONE);
        let style = COMBAT_FEEDBACK.burst_style(false);
        assert_eq!(style.impact_rgb, COMBAT_FEEDBACK.horizon_rgb);
        assert_eq!(style.star_points, COMBAT_FEEDBACK.burst_points);
        assert_eq!(style.impact_lifetime_ticks, COMBAT_FEEDBACK.burst_ticks);
        assert_eq!(
            COMBAT_FEEDBACK.burst_style(true).impact_rgb,
            COMBAT_FEEDBACK.zenith_rgb
        );
        let spark = COMBAT_FEEDBACK.spark_style(FlashKind::Crit, true);
        assert_eq!(spark.impact_rgb, COMBAT_FEEDBACK.zenith_rgb);
        assert_eq!(
            spark.star_points,
            COMBAT_FEEDBACK.spark_points | STAR_NO_RING
        );
        assert!(
            spark.glow_scale_q8
                > COMBAT_FEEDBACK
                    .spark_style(FlashKind::Hit, true)
                    .glow_scale_q8
        );
        assert!(
            spark.glow_scale_q8 < style.glow_scale_q8,
            "a hit is smaller"
        );
        assert!(COMBAT_FEEDBACK.spark_radius < COMBAT_FEEDBACK.burst_radius);
    }
}
