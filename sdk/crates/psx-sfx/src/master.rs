//! Master volumes and the music duck.
//!
//! Six games each scale their sound effects by an options-menu setting in six
//! different ways. [`Master`] is the one copy: an exact rational for the effects
//! level, the same for music, and a ducking gain that lets an important sound
//! pull the music down and release it afterwards.
//!
//! The mixer applies the effects ratio at the point a level reaches the chip and
//! remembers the unscaled level, so changing the setting while sounds play
//! re-scales them immediately. Music is not routed here: [`Master::music_level_q15`]
//! returns the value, and the game writes it to the CD volume register.

use crate::{SPU_LEVEL_MAX, SPU_LEVEL_MIN};

/// An exact fraction `num / den`.
///
/// Exact rather than Q12 so a game that today computes `x * full / base`
/// keeps its rounding: [`apply`](Self::apply) multiplies first and divides once,
/// truncating toward zero.
///
/// # Example
///
/// ```
/// use psx_sfx::master::Ratio;
///
/// let two_thirds = Ratio::new(2, 3);
/// assert_eq!(two_thirds.apply(100), 66);
/// assert_eq!(Ratio::UNITY.apply(-5), -5);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    num: u16,
    den: u16,
}

impl Ratio {
    /// One: leaves a level unchanged.
    pub const UNITY: Self = Self { num: 1, den: 1 };

    /// Silence: every level becomes zero.
    pub const MUTE: Self = Self { num: 0, den: 1 };

    /// The fraction `num / den`.
    ///
    /// # Panics
    /// If `den` is zero. In a `const` this is a build error.
    pub const fn new(num: u16, den: u16) -> Self {
        assert!(den != 0, "Ratio denominator must not be zero");
        Self { num, den }
    }

    /// Numerator.
    pub const fn num(self) -> u16 {
        self.num
    }

    /// Denominator.
    pub const fn den(self) -> u16 {
        self.den
    }

    /// `value * num / den`, truncated toward zero.
    ///
    /// ```
    /// # use psx_sfx::master::Ratio;
    /// assert_eq!(Ratio::new(1, 4).apply(-10), -2);
    /// ```
    ///
    /// `value` must keep `value * num` inside `i32`, which holds for any SPU
    /// level (`i16`) and any `num` up to `u16::MAX`.
    pub const fn apply(self, value: i32) -> i32 {
        value * self.num as i32 / self.den as i32
    }
}

/// Effects and music master volumes plus the music duck.
///
/// # Example
///
/// ```
/// use psx_sfx::master::{Master, Ratio};
///
/// let mut m = Master::new();
/// m.set_sfx(Ratio::new(1, 2));
/// assert_eq!(m.scale_level(8000), 4000);
///
/// // A protected sound asks for the music to drop to a quarter, then lets go.
/// m.set_duck_rates(1024, 128);
/// m.step_duck(1024);
/// assert_eq!(m.duck_gain_q12(), 3072);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Master {
    sfx: Ratio,
    music: Ratio,
    duck: u16,
    attack: u16,
    release: u16,
}

/// Duck gain meaning "music untouched": Q12 one.
const DUCK_NONE: u16 = 4096;

impl Master {
    /// Everything at unity, no duck, a duck that moves the whole range in one
    /// tick until [`set_duck_rates`](Self::set_duck_rates) says otherwise.
    pub const fn new() -> Self {
        Self {
            sfx: Ratio::UNITY,
            music: Ratio::UNITY,
            duck: DUCK_NONE,
            attack: DUCK_NONE,
            release: DUCK_NONE,
        }
    }

    /// Set the effects master.
    ///
    /// ```
    /// # use psx_sfx::master::{Master, Ratio};
    /// let mut m = Master::new();
    /// m.set_sfx(Ratio::new(7, 10));
    /// assert_eq!(m.sfx(), Ratio::new(7, 10));
    /// ```
    pub fn set_sfx(&mut self, sfx: Ratio) {
        self.sfx = sfx;
    }

    /// Set the music master.
    ///
    /// ```
    /// # use psx_sfx::master::{Master, Ratio};
    /// let mut m = Master::new();
    /// m.set_music(Ratio::MUTE);
    /// assert_eq!(m.music_level_q15(), 0);
    /// ```
    pub fn set_music(&mut self, music: Ratio) {
        self.music = music;
    }

    /// The effects master.
    pub const fn sfx(&self) -> Ratio {
        self.sfx
    }

    /// The music master.
    pub const fn music(&self) -> Ratio {
        self.music
    }

    /// Scale one signed SPU level by the effects master and clamp it to the
    /// range the chip accepts.
    ///
    /// ```
    /// # use psx_sfx::master::{Master, Ratio};
    /// let mut m = Master::new();
    /// m.set_sfx(Ratio::new(3, 2));
    /// assert_eq!(m.scale_level(10000), 15000);
    /// assert_eq!(m.scale_level(12000), 0x3FFF); // clamped to the chip's range
    /// ```
    pub const fn scale_level(&self, level: i16) -> i16 {
        let v = self.sfx.apply(level as i32);
        if v > SPU_LEVEL_MAX as i32 {
            SPU_LEVEL_MAX
        } else if v < SPU_LEVEL_MIN as i32 {
            SPU_LEVEL_MIN
        } else {
            v as i16
        }
    }

    /// How fast the duck moves, in Q12 gain per tick: `attack` when the music
    /// is being pulled down, `release` when it is coming back.
    ///
    /// ```
    /// # use psx_sfx::master::Master;
    /// let mut m = Master::new();
    /// m.set_duck_rates(2048, 256);
    /// m.step_duck(0);
    /// assert_eq!(m.duck_gain_q12(), 2048);
    /// ```
    pub fn set_duck_rates(&mut self, attack: u16, release: u16) {
        self.attack = attack;
        self.release = release;
    }

    /// Move the duck one tick toward `target` (Q12; `4096` is no duck).
    /// [`Mixer::tick`](crate::mixer::Mixer::tick) calls this with the deepest
    /// duck any live sound is asking for.
    ///
    /// ```
    /// # use psx_sfx::master::Master;
    /// let mut m = Master::new(); // rates default to the whole range per tick
    /// m.step_duck(1000);
    /// assert_eq!(m.duck_gain_q12(), 1000);
    /// m.step_duck(4096);
    /// assert_eq!(m.duck_gain_q12(), 4096);
    /// ```
    pub fn step_duck(&mut self, target: u16) {
        let target = target.min(DUCK_NONE);
        if self.duck > target {
            self.duck = self.duck.saturating_sub(self.attack).max(target);
        } else if self.duck < target {
            self.duck = self.duck.saturating_add(self.release).min(target);
        }
    }

    /// The current duck gain, Q12 (`4096` = music at full level).
    ///
    /// ```
    /// # use psx_sfx::master::Master;
    /// assert_eq!(Master::new().duck_gain_q12(), 4096);
    /// ```
    pub const fn duck_gain_q12(&self) -> u16 {
        self.duck
    }

    /// Has the duck reached `target`?
    ///
    /// ```
    /// # use psx_sfx::master::Master;
    /// assert!(Master::new().duck_settled(4096));
    /// ```
    pub const fn duck_settled(&self, target: u16) -> bool {
        self.duck == target
    }

    /// The music level to write to the CD volume register: the music master
    /// times the duck, as signed Q15 (`0x7FFF` is full scale).
    ///
    /// ```
    /// # use psx_sfx::master::{Master, Ratio};
    /// let mut m = Master::new();
    /// m.set_music(Ratio::new(1, 2));
    /// assert_eq!(m.music_level_q15(), 0x3FFF);
    /// ```
    pub const fn music_level_q15(&self) -> i16 {
        ((self.music.apply(0x7fff) * self.duck as i32) >> 12) as i16
    }
}

impl Default for Master {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ratio_multiplies_first_and_truncates_toward_zero() {
        assert_eq!(Ratio::new(1, 3).apply(100), 33);
        assert_eq!(Ratio::new(1, 3).apply(-100), -33);
        assert_eq!(Ratio::new(8190, 0x3fff).apply(16383), 8190);
        assert_eq!(Ratio::MUTE.apply(12345), 0);
        // WipEout's `x * full / base` shape: the same integer result.
        for x in [-16384i32, -1, 0, 1, 777, 16383] {
            assert_eq!(Ratio::new(3, 7).apply(x), x * 3 / 7);
        }
    }

    #[test]
    #[should_panic(expected = "denominator")]
    fn a_zero_denominator_is_rejected() {
        let _ = Ratio::new(1, 0);
    }

    #[test]
    fn levels_are_clamped_to_the_spu_range_after_scaling() {
        let mut m = Master::new();
        m.set_sfx(Ratio::new(4, 1));
        assert_eq!(m.scale_level(0x3000), 0x3fff);
        assert_eq!(m.scale_level(-0x3000), -0x4000);
        assert_eq!(m.scale_level(1000), 4000);
        assert_eq!(m.scale_level(i16::MIN), -0x4000);
    }

    #[test]
    fn the_duck_attacks_and_releases_at_their_own_rates_and_stops_on_target() {
        let mut m = Master::new();
        m.set_duck_rates(1000, 100);
        let mut seen = [0u16; 6];
        for s in seen.iter_mut() {
            m.step_duck(2000);
            *s = m.duck_gain_q12();
        }
        assert_eq!(seen, [3096, 2096, 2000, 2000, 2000, 2000]);
        m.step_duck(4096);
        assert_eq!(m.duck_gain_q12(), 2100);
        for _ in 0..30 {
            m.step_duck(4096);
        }
        assert_eq!(m.duck_gain_q12(), 4096);
        assert!(m.duck_settled(4096));
    }

    #[test]
    fn music_level_is_the_master_times_the_duck_in_signed_q15() {
        let mut m = Master::new();
        assert_eq!(m.music_level_q15(), 0x7fff);
        m.set_music(Ratio::new(1, 2));
        assert_eq!(m.music_level_q15(), 0x3fff);
        m.set_duck_rates(4096, 4096);
        m.step_duck(2048);
        assert_eq!(m.music_level_q15(), 0x1fff);
    }
}
