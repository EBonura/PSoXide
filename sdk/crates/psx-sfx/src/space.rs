//! Where a sound is, and what that does to its two ears.
//!
//! Four games spatialise sound today with four hand-written formulas, and
//! three of them have a private square root. This module is the shared version:
//! a [`Curve`] turns distance into a level, a [`PanLaw`] splits the level over
//! the two ears, a [`ListenerRule`] picks which of several listeners (a split
//! screen) a sound is heard from, and [`gains`] runs the whole chain.
//!
//! Everything is `i32`. Distances are clamped per axis before they are squared,
//! so the sum of squares stays below `2^31` and no 64-bit arithmetic is needed
//! (the PS1 has none, and a 64-bit divide costs hundreds of cycles).
//!
//! # Coordinates
//!
//! The horizontal plane is `x` and `z`; `y` is up and only adds to distance.
//! A listener with heading `0` faces `+z` with its right ear toward `+x`, and a
//! positive heading turns toward `+x`. A game with the other handedness negates
//! the `x` axis (or the heading) once when it builds its [`Listener`] and
//! [`Emitter`] values.
//!
//! # Example
//!
//! ```
//! use psx_sfx::space::*;
//!
//! let space = Space::new(0, PanLaw::EqualPower(Center::Unity));
//! let ears = Listeners::new([Listener::new([0, 0, 0], 0)]);
//! // A sound 400 units ahead and 400 to the right.
//! let source = Emitter::new([400, 0, 400]);
//! let curve = Curve::linear_clip(0x3FFF, 2000);
//! let g = gains(&space, &ears, ListenerRule::Loudest, &source, &curve).unwrap();
//! assert!(g.right > g.left);
//! ```

use crate::{SPU_LEVEL_MAX, SPU_LEVEL_MIN};
use psx_math::int32::isqrt_i32;
use psx_math::{cos_q12, sin_q12, SIN_TABLE};

/// Largest per-axis distance, after the world shift, that is squared as is.
/// Three of these squared sum to less than `2^31`.
pub const AXIS_LIMIT: i32 = 16383;

/// Full right in [`PanLaw`] input: Q12 one.
pub const PAN_RIGHT: i32 = 4096;

/// Quake's attenuation classes, as the `Curve` constructors take them.
///
/// # Example
///
/// ```
/// use psx_sfx::space::{Attn, Curve};
///
/// // A static ambient hears half as far as an idle one in Quake terms.
/// let c = Curve::quake(Attn::Static, 0x3FFF);
/// assert_eq!(c.level(0), 0x3FFF);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attn {
    /// Quake `ATTN_NORM` (1.0): silent at 1000 units.
    Norm,
    /// Quake `ATTN_IDLE` (2.0): silent at 500 units.
    Idle,
    /// Quake `ATTN_STATIC` (3.0): silent at 333 units.
    Static,
}

impl Attn {
    /// The attenuation in thousandths.
    pub const fn milli(self) -> u32 {
        match self {
            Attn::Norm => 1000,
            Attn::Idle => 2000,
            Attn::Static => 3000,
        }
    }
}

/// GoldSrc `ATTN_NORM`, in thousandths, for [`Curve::goldsrc`].
pub const GOLDSRC_NORM: u32 = 800;
/// GoldSrc `ATTN_IDLE`, in thousandths.
pub const GOLDSRC_IDLE: u32 = 2000;
/// GoldSrc `ATTN_STATIC`, in thousandths.
pub const GOLDSRC_STATIC: u32 = 1250;

/// Distance to level.
///
/// The level domain is the SPU's own, so the output of
/// [`level`](Self::level) can go straight into a [`PanLaw`] and then to the
/// chip. `dist` is the distance after the world shift, `0` or more.
///
/// # Example
///
/// ```
/// use psx_sfx::space::Curve;
///
/// let c = Curve::linear_clip(10000, 1000);
/// assert_eq!(c.level(0), 10000);
/// assert!(c.level(500).abs_diff(5000) <= 8);
/// assert_eq!(c.level(1000), 0);
/// assert_eq!(c.level(5000), 0);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// `peak - (max(d - start, 0) * slope >> shift)`, clamped to
    /// `floor..=peak`. The shape of Quake, GoldSrc and WipEout attenuation.
    Ramp {
        /// Level at and inside `start`.
        peak: i16,
        /// Distance the fall-off begins at.
        start: i32,
        /// Level lost per unit of distance, in units of `1 / 2^shift`. Sixteen
        /// bits so the product with a distance (under `2^15`) cannot overflow
        /// and needs no saturating multiply, which on the R3000 is a library
        /// call.
        slope: u16,
        /// Fixed-point shift for `slope`; below 32.
        shift: u8,
        /// Lowest level the curve returns.
        floor: i16,
    },
    /// `peak * unit / (unit + d)`: the inverse-distance law, one divide.
    /// `unit` is the distance at which the level has halved; keep it under
    /// 131071 so the product fits `i32`.
    Hyper {
        /// Level at distance zero.
        peak: i16,
        /// Distance of the half-level point.
        unit: i32,
    },
    /// Sixteen linear segments of `2^step_shift` distance units, no divide.
    /// `pts[i]` is the level at `i << step_shift`; past the last point the
    /// level stays at `pts[16]`. `step_shift` is at most 14.
    Table {
        /// log2 of the segment length.
        step_shift: u8,
        /// Levels at the segment boundaries.
        pts: [u16; 17],
    },
    /// The same level at any distance.
    Flat(i16),
}

impl Curve {
    /// A ramp from `peak` that reaches silence at distance `clip`, with the
    /// slope computed once at construction.
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// assert_eq!(Curve::linear_clip(8000, 2000).level(0), 8000);
    /// assert_eq!(Curve::linear_clip(8000, 2000).level(2000), 0);
    /// ```
    ///
    /// The slope is held in 16 bits, so clips under 256 units fall off at the
    /// steepest slope there is (the level is gone in 256 units) rather than at
    /// their true rate. Distances are in the curve's own units after the world
    /// shift, so a clip that short is a shift away from being useful anyway.
    pub const fn linear_clip(peak: i16, clip: i32) -> Self {
        let clip = if clip < 1 { 1 } else { clip };
        // Rounded up, so the level is exactly zero at `clip`, not 1.
        let slope = (((peak as i32) << 10) + clip - 1) / clip;
        let slope = if slope > u16::MAX as i32 {
            u16::MAX
        } else if slope < 0 {
            0
        } else {
            slope as u16
        };
        Curve::Ramp {
            peak,
            start: 0,
            slope,
            shift: 10,
            floor: 0,
        }
    }

    /// The Quake and GoldSrc law: `peak * (1 - d * attn / 1000)`, so silence at
    /// `1000 / attn` units. `attn_milli` is the attenuation in thousandths;
    /// `0` is "no attenuation" and gives a [`Curve::Flat`].
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// // Attenuation 2.0: silent at 500 units.
    /// assert_eq!(Curve::attenuated(8000, 2000).level(500), 0);
    /// assert_eq!(Curve::attenuated(8000, 0), Curve::Flat(8000));
    /// ```
    pub const fn attenuated(peak: i16, attn_milli: u32) -> Self {
        if attn_milli == 0 {
            return Curve::Flat(peak);
        }
        Self::linear_clip(peak, (1_000_000 / attn_milli) as i32)
    }

    /// Quake `S_Spatialize`'s distance scale for an attenuation class.
    ///
    /// ```
    /// # use psx_sfx::space::{Attn, Curve};
    /// assert_eq!(Curve::quake(Attn::Idle, 8000).level(500), 0);
    /// ```
    pub const fn quake(attn: Attn, peak: i16) -> Self {
        Self::attenuated(peak, attn.milli())
    }

    /// GoldSrc's linear gain. `attn_milli` is the attenuation in thousandths
    /// ([`GOLDSRC_NORM`], [`GOLDSRC_IDLE`], [`GOLDSRC_STATIC`], or any other);
    /// `0` is `ATTN_NONE`.
    ///
    /// ```
    /// # use psx_sfx::space::{Curve, GOLDSRC_NORM};
    /// // ATTN_NORM 0.8: silent at 1250 units.
    /// assert_eq!(Curve::goldsrc(8000, GOLDSRC_NORM).level(1250), 0);
    /// ```
    pub const fn goldsrc(peak: i16, attn_milli: u32) -> Self {
        Self::attenuated(peak, attn_milli)
    }

    /// WipEout's one-shot law: `0x2FFF - d`, with `d` in the game's own
    /// eighth-of-a-unit distance.
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// assert_eq!(Curve::wipeout().level(1000), 0x2FFF - 1000);
    /// ```
    pub const fn wipeout() -> Self {
        Curve::Ramp {
            peak: 0x2fff,
            start: 0,
            slope: 1,
            shift: 0,
            floor: 0,
        }
    }

    /// WipEout's crowd law: `0x2FFF - 4 * d`.
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// assert_eq!(Curve::wipeout_crowd().level(1000), 0x2FFF - 4000);
    /// ```
    pub const fn wipeout_crowd() -> Self {
        Curve::Ramp {
            peak: 0x2fff,
            start: 0,
            slope: 4,
            shift: 0,
            floor: 0,
        }
    }

    /// The inverse-distance law with its half-level point at `unit`.
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// assert_eq!(Curve::inverse(8000, 300).level(300), 4000);
    /// ```
    pub const fn inverse(peak: i16, unit: i32) -> Self {
        let unit = if unit < 1 {
            1
        } else if unit > 131_071 {
            131_071
        } else {
            unit
        };
        Curve::Hyper { peak, unit }
    }

    /// The level at distance `dist`.
    ///
    /// ```
    /// # use psx_sfx::space::Curve;
    /// assert_eq!(Curve::Flat(500).level(12345), 500);
    /// ```
    #[inline]
    pub fn level(&self, dist: i32) -> i16 {
        let dist = dist.max(0);
        match *self {
            Curve::Ramp {
                peak,
                start,
                slope,
                shift,
                floor,
            } => {
                // dist is at most 28376 and slope under 2^16: no overflow.
                let beyond = (dist - start).clamp(0, 32767);
                let fall = (beyond * slope as i32) >> (shift & 31);
                let v = peak as i32 - fall;
                v.clamp(floor as i32, peak as i32) as i16
            }
            Curve::Hyper { peak, unit } => {
                // peak < 2^14 and unit <= 131071 (see `Curve::inverse`).
                ((peak as i32 * unit) / (unit + dist)) as i16
            }
            Curve::Table { step_shift, pts } => {
                let s = (step_shift as u32).min(14);
                let seg = (dist >> s) as usize;
                if seg >= 16 {
                    return pts[16].min(SPU_LEVEL_MAX as u16) as i16;
                }
                let a = pts[seg] as i32;
                let b = pts[seg + 1] as i32;
                let frac = dist & ((1 << s) - 1);
                (a + (((b - a) * frac) >> s)) as i16
            }
            Curve::Flat(v) => v,
        }
    }
}

/// Which centre loss a [`PanLaw::EqualPower`] has.
///
/// # Example
///
/// ```
/// use psx_sfx::space::{Center, PanLaw};
///
/// let (l, r) = PanLaw::EqualPower(Center::Unity).apply(8000, 0);
/// assert_eq!((l, r), (8000, 8000));
/// let (l, _) = PanLaw::EqualPower(Center::Minus3Db).apply(8000, 0);
/// assert!(l < 8000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Center {
    /// A centred sound keeps its full level on both ears and the louder side of
    /// an off-centre sound clamps at that level: a loud mono sound loses
    /// nothing, but power is no longer exactly constant near the middle.
    Unity,
    /// The textbook law: a centred sound is 3 dB down on each ear, and the sum
    /// of squares is constant across the whole pan range.
    Minus3Db,
}

/// How a level is split over two ears.
///
/// `pan` is Q12 from [`-PAN_RIGHT`](PAN_RIGHT) (hard left) to `PAN_RIGHT` (hard
/// right).
///
/// # Example
///
/// ```
/// use psx_sfx::space::{PanLaw, PAN_RIGHT};
///
/// // Quake's law: each ear gets (1 -+ pan) of the level.
/// let (l, r) = PanLaw::Dot.apply(1000, PAN_RIGHT / 2);
/// assert_eq!((l, r), (500, 1500));
/// ```
#[derive(Clone, Copy, Debug)]
pub enum PanLaw {
    /// Both ears get the level: a mono source.
    Off,
    /// WipEout's constant-sum law. The ears get `v + d` and `v - d` with
    /// `d = (v * -pan) >> 13`, so a pan of `PAN_RIGHT` is a quarter turn and
    /// moves half the level across. With `pan = -4a` this is bit for bit the
    /// retail `v * a >> 11` split, including the floor rounding.
    Sum,
    /// Quake's law: the left ear gets `v * (1 - pan)`, the right `v * (1 + pan)`.
    Dot,
    /// Constant power: `left = cos`, `right = sin` of a quarter turn swept by
    /// the pan, from the 256-entry sine table in `psx-math`.
    EqualPower(Center),
    /// The caller's own law, for retail-exact reproduction. Receives the level
    /// and the pan and returns the two ear levels.
    Custom(fn(i32, i32) -> (i32, i32)),
}

impl PanLaw {
    /// Split `level` over two ears for `pan`. The result is not clamped to the
    /// SPU range; [`gains`] does that.
    ///
    /// ```
    /// # use psx_sfx::space::PanLaw;
    /// assert_eq!(PanLaw::Off.apply(900, 4096), (900, 900));
    /// ```
    pub fn apply(&self, level: i32, pan: i32) -> (i32, i32) {
        let pan = pan.clamp(-PAN_RIGHT, PAN_RIGHT);
        match *self {
            PanLaw::Off => (level, level),
            PanLaw::Sum => {
                let d = (level * -pan) >> 13;
                (level + d, level - d)
            }
            PanLaw::Dot => (
                (level * (PAN_RIGHT - pan)) >> 12,
                (level * (PAN_RIGHT + pan)) >> 12,
            ),
            PanLaw::EqualPower(centre) => {
                // Pan -4096..4096 sweeps a quarter turn of the sine table, 64
                // entries of 128 pan units each: left = cos, right = sin of the
                // swept angle, read straight from the table with one linear
                // interpolation each (the table's entries are the angles
                // 0, 16, 32, ... in Q0.12, so index i is pan 128 * i - 4096).
                let p = pan + PAN_RIGHT;
                let i = (p >> 7) as usize;
                let f = p & 127;
                let lerp = |a: i16, b: i16| a as i32 + (((b as i32 - a as i32) * f) >> 7);
                let mut cl = lerp(SIN_TABLE[(i + 64) & 255], SIN_TABLE[(i + 65) & 255]);
                let mut cr = lerp(SIN_TABLE[i], SIN_TABLE[i + 1]);
                if centre == Center::Unity {
                    // sqrt(2) in Q12, rounded up (5793 would leave the table's
                    // 2896 at 4095): lifts the centre from -3 dB to exactly
                    // unity and lets the louder side clamp.
                    cl = ((cl * 5794) >> 12).min(4096);
                    cr = ((cr * 5794) >> 12).min(4096);
                }
                ((level * cl) >> 12, (level * cr) >> 12)
            }
            PanLaw::Custom(f) => f(level, pan),
        }
    }
}

/// How a world position becomes a distance: one place for the world unit to
/// ear unit shift, the pan law and the cull threshold.
///
/// # Example
///
/// ```
/// use psx_sfx::space::{PanLaw, Space};
///
/// // World units are 1/8 of the unit the curve is written in, and sounds
/// // under 100 are not worth a voice.
/// let space = Space::new(3, PanLaw::Off).with_cull(100);
/// assert_eq!(space.cull(), 100);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Space {
    shift: u8,
    law: PanLaw,
    cull: i16,
}

impl Space {
    /// Distances are `(emitter - listener) >> shift` per axis.
    pub const fn new(shift: u8, law: PanLaw) -> Self {
        Self {
            shift,
            law,
            cull: 0,
        }
    }

    /// Sounds whose curve level is at or below `cull` are reported as
    /// inaudible ([`gains`] returns `None`) before any pan work is done.
    ///
    /// ```
    /// # use psx_sfx::space::{PanLaw, Space};
    /// assert_eq!(Space::new(0, PanLaw::Off).with_cull(40).cull(), 40);
    /// ```
    pub const fn with_cull(mut self, cull: i16) -> Self {
        self.cull = cull;
        self
    }

    /// The cull threshold.
    pub const fn cull(&self) -> i16 {
        self.cull
    }
}

/// Where the ears are.
///
/// # Example
///
/// ```
/// use psx_sfx::space::Listener;
///
/// let l = Listener::new([10, 0, 20], 1024); // facing +x
/// assert_eq!(l.heading_q12(), 1024);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listener {
    pos: [i32; 3],
    heading_q12: u16,
    // sin and cos of the heading, Q12, worked out once here rather than for
    // every emitter placed against this listener.
    sin: i16,
    cos: i16,
}

impl Listener {
    /// A listener at `pos` facing `heading_q12` (4096 to a turn, see the module
    /// docs for the direction convention). Works out the heading's sine and
    /// cosine once, so build it when the camera moves, not per emitter.
    pub fn new(pos: [i32; 3], heading_q12: u16) -> Self {
        Self {
            pos,
            heading_q12,
            sin: sin_q12(heading_q12) as i16,
            cos: cos_q12(heading_q12) as i16,
        }
    }

    /// World position.
    pub const fn pos(&self) -> [i32; 3] {
        self.pos
    }

    /// Yaw, Q12 turns.
    pub const fn heading_q12(&self) -> u16 {
        self.heading_q12
    }
}

/// Where a sound comes from.
///
/// # Example
///
/// ```
/// use psx_sfx::space::Emitter;
///
/// assert_eq!(Emitter::new([1, 2, 3]).pos(), [1, 2, 3]);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Emitter {
    pos: [i32; 3],
}

impl Emitter {
    /// An emitter at `pos`.
    pub const fn new(pos: [i32; 3]) -> Self {
        Self { pos }
    }

    /// World position.
    pub const fn pos(&self) -> [i32; 3] {
        self.pos
    }
}

/// The listeners in a scene: one, or two for a split screen.
///
/// # Example
///
/// ```
/// use psx_sfx::space::{Listener, Listeners};
///
/// let ears = Listeners::new([Listener::new([0; 3], 0), Listener::new([50, 0, 0], 0)]);
/// assert_eq!(ears.len(), 2);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Listeners<const N: usize>([Listener; N]);

impl<const N: usize> Listeners<N> {
    /// Wrap `listeners`.
    pub const fn new(listeners: [Listener; N]) -> Self {
        Self(listeners)
    }

    /// How many listeners there are.
    pub const fn len(&self) -> usize {
        N
    }

    /// Is there no listener at all?
    pub const fn is_empty(&self) -> bool {
        N == 0
    }

    /// Replace listener `index`, for a camera that moved this frame.
    ///
    /// ```
    /// # use psx_sfx::space::{Listener, Listeners};
    /// let mut ears = Listeners::new([Listener::new([0; 3], 0)]);
    /// ears.set(0, Listener::new([5, 0, 5], 1024));
    /// ```
    pub fn set(&mut self, index: usize, listener: Listener) {
        self.0[index] = listener;
    }
}

/// Which listener a sound is heard from when there are several. There is one
/// television and one stereo image, so there is no true answer, only a policy.
///
/// # Example
///
/// ```
/// use psx_sfx::space::*;
///
/// let space = Space::new(0, PanLaw::Off);
/// let ears = Listeners::new([Listener::new([0; 3], 0), Listener::new([1000, 0, 0], 0)]);
/// let curve = Curve::linear_clip(0x3FFF, 4000);
/// let near_second = Emitter::new([900, 0, 0]);
/// let g = gains(&space, &ears, ListenerRule::Nearest, &near_second, &curve).unwrap();
/// assert_eq!(g.listener, 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerRule {
    /// Evaluate every listener and keep the one the sound is loudest to
    /// (`left + right`; ties go to the first). WipEout's rule for one-shots.
    Loudest,
    /// The nearest listener decides. Keeps a loop's pan stable instead of
    /// flipping between listeners as their levels cross.
    Nearest,
    /// A virtual listener at the mean position and heading of all of them, for
    /// scene-wide events (a goal, a crowd roar) that neither player owns.
    Centroid,
}

/// The result of placing a sound: the two ear levels and where they came from.
///
/// # Example
///
/// ```
/// use psx_sfx::space::*;
///
/// let space = Space::new(0, PanLaw::Off);
/// let ears = Listeners::new([Listener::new([0; 3], 0)]);
/// let g = gains(&space, &ears, ListenerRule::Loudest, &Emitter::new([0, 0, 100]), &Curve::Flat(900)).unwrap();
/// assert_eq!((g.left, g.right, g.dist), (900, 900, 100));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gains {
    /// Left ear level, SPU range.
    pub left: i16,
    /// Right ear level, SPU range.
    pub right: i16,
    /// Index of the listener the sound is heard from (`0` for a centroid).
    pub listener: u8,
    /// Distance to that listener, after the world shift.
    pub dist: i32,
}

impl Gains {
    /// The louder ear, floored at zero: what a voice allocator ranks by.
    ///
    /// ```
    /// # use psx_sfx::space::Gains;
    /// let g = Gains { left: -3, right: 700, listener: 0, dist: 10 };
    /// assert_eq!(g.audibility(), 700);
    /// ```
    pub fn audibility(&self) -> i32 {
        (self.left.max(self.right) as i32).max(0)
    }
}

/// Clamp one axis difference into the range that can be squared safely.
#[inline(always)]
fn axis(a: i32, b: i32, shift: u8) -> i32 {
    (a.wrapping_sub(b) >> (shift & 31)).clamp(-AXIS_LIMIT, AXIS_LIMIT)
}

#[inline(always)]
fn clamp_spu(v: i32) -> i16 {
    v.clamp(SPU_LEVEL_MIN as i32, SPU_LEVEL_MAX as i32) as i16
}

#[inline(always)]
fn offsets(space: &Space, l: &Listener, e: &Emitter) -> [i32; 3] {
    [
        axis(e.pos[0], l.pos[0], space.shift),
        axis(e.pos[1], l.pos[1], space.shift),
        axis(e.pos[2], l.pos[2], space.shift),
    ]
}

/// Place `emitter` for one listener.
#[inline(always)]
fn evaluate(
    space: &Space,
    index: u8,
    listener: &Listener,
    emitter: &Emitter,
    curve: &Curve,
) -> Option<Gains> {
    let d = offsets(space, listener, emitter);
    let dist = isqrt_i32(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
    let level = curve.level(dist);
    if level <= space.cull {
        return None;
    }
    let pan = match space.law {
        PanLaw::Off => 0,
        _ if dist == 0 => 0,
        _ => {
            // right = (cos h, -sin h) in (x, z); pan = right . (d / |d|), Q12.
            let dot = d[0] * listener.cos as i32 - d[2] * listener.sin as i32;
            dot / dist
        }
    };
    let (l, r) = space.law.apply(level as i32, pan);
    Some(Gains {
        left: clamp_spu(l),
        right: clamp_spu(r),
        listener: index,
        dist,
    })
}

/// Place `emitter` for a scene of listeners under `rule`, returning the ear
/// levels, or `None` when the sound is inaudible and should not take a voice.
///
/// Cost is one full placement per listener for [`ListenerRule::Loudest`], and
/// one placement plus a squared-distance scan for the other two rules.
///
/// # Example
///
/// ```
/// use psx_sfx::space::*;
///
/// let space = Space::new(0, PanLaw::Dot).with_cull(50);
/// let ears = Listeners::new([Listener::new([0; 3], 0)]);
/// let curve = Curve::quake(Attn::Norm, 0x3FFF);
/// // Beyond 1000 units a Norm sound is gone.
/// assert!(gains(&space, &ears, ListenerRule::Loudest, &Emitter::new([0, 0, 1200]), &curve).is_none());
/// ```
pub fn gains<const N: usize>(
    space: &Space,
    listeners: &Listeners<N>,
    rule: ListenerRule,
    emitter: &Emitter,
    curve: &Curve,
) -> Option<Gains> {
    if N == 0 {
        return None;
    }
    let ls = &listeners.0;
    if N == 1 {
        return evaluate(space, 0, &ls[0], emitter, curve);
    }
    match rule {
        ListenerRule::Loudest => {
            let mut best: Option<Gains> = None;
            for (i, l) in ls.iter().enumerate() {
                if let Some(g) = evaluate(space, i as u8, l, emitter, curve) {
                    let better = match best {
                        None => true,
                        Some(b) => g.left as i32 + g.right as i32 > b.left as i32 + b.right as i32,
                    };
                    if better {
                        best = Some(g);
                    }
                }
            }
            best
        }
        ListenerRule::Nearest => {
            let mut at = 0usize;
            let mut best = i32::MAX;
            for (i, l) in ls.iter().enumerate() {
                let d = offsets(space, l, emitter);
                let sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                if sq < best {
                    best = sq;
                    at = i;
                }
            }
            evaluate(space, at as u8, &ls[at], emitter, curve)
        }
        ListenerRule::Centroid => {
            // Offsets from the first listener, so the sum cannot overflow for
            // listeners that are near each other in world terms.
            let base = ls[0].pos;
            let mut sum = [0i32; 3];
            let (mut sx, mut sz) = (0i32, 0i32);
            for l in ls.iter() {
                for (s, (p, b)) in sum.iter_mut().zip(l.pos.iter().zip(base.iter())) {
                    *s = s.wrapping_add(p.wrapping_sub(*b));
                }
                sx += sin_q12(l.heading_q12);
                sz += cos_q12(l.heading_q12);
            }
            let n = N as i32;
            let mean = Listener::new(
                [
                    base[0].wrapping_add(sum[0] / n),
                    base[1].wrapping_add(sum[1] / n),
                    base[2].wrapping_add(sum[2] / n),
                ],
                psx_math::atan2_q12(sx, sz),
            );
            evaluate(space, 0, &mean, emitter, curve)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(law: PanLaw) -> (Space, Listeners<1>) {
        (
            Space::new(0, law),
            Listeners::new([Listener::new([0, 0, 0], 0)]),
        )
    }

    fn place(law: PanLaw, at: [i32; 3], curve: &Curve) -> Option<Gains> {
        let (sp, ls) = one(law);
        gains(&sp, &ls, ListenerRule::Loudest, &Emitter::new(at), curve)
    }

    #[test]
    fn distance_is_euclidean() {
        let g = place(PanLaw::Off, [3000, 4000, 0], &Curve::Flat(1)).unwrap();
        assert_eq!(g.dist, 5000);
        let g = place(PanLaw::Off, [1000, 2000, 2000], &Curve::Flat(1)).unwrap();
        assert_eq!(g.dist, 3000);
        assert_eq!(
            place(PanLaw::Off, [0, 0, 0], &Curve::Flat(1)).unwrap().dist,
            0
        );
    }

    #[test]
    fn the_world_shift_scales_distance_before_the_curve() {
        let space = Space::new(3, PanLaw::Off);
        let ls = Listeners::new([Listener::new([0; 3], 0)]);
        let g = gains(
            &space,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([8000, 0, 0]),
            &Curve::Flat(1),
        );
        assert_eq!(g.unwrap().dist, 1000);
    }

    #[test]
    fn extreme_positions_neither_overflow_nor_wrap_into_nearness() {
        let space = Space::new(0, PanLaw::EqualPower(Center::Unity));
        let ls = Listeners::new([Listener::new([i32::MAX, i32::MIN, 0], 0)]);
        for far in [
            [i32::MIN, i32::MAX, i32::MAX],
            [i32::MAX, i32::MAX, i32::MIN],
            [0, 0, 0],
        ] {
            let g = gains(
                &space,
                &ls,
                ListenerRule::Loudest,
                &Emitter::new(far),
                &Curve::linear_clip(0x3fff, 20000),
            );
            // Whatever the answer, it is a valid, finite one.
            if let Some(g) = g {
                assert!(g.dist <= 28_377);
            }
        }
        // Per-axis clamping: 100000 away on one axis is no further than 16383.
        let g = place(PanLaw::Off, [100_000, 0, 0], &Curve::Flat(1)).unwrap();
        assert_eq!(g.dist, AXIS_LIMIT);
    }

    #[test]
    fn ramp_matches_wipeouts_retail_law() {
        let one_shot = Curve::wipeout();
        let crowd = Curve::wipeout_crowd();
        for d in [0, 1, 500, 1144, 5000, 12287, 12288, 30000] {
            assert_eq!(
                one_shot.level(d),
                (0x2fff - d).max(0) as i16,
                "one-shot {d}"
            );
            assert_eq!(crowd.level(d), (0x2fff - 4 * d).max(0) as i16, "crowd {d}");
        }
    }

    #[test]
    fn quake_attenuation_follows_the_linear_law_for_every_class() {
        for (attn, clip) in [
            (Attn::Norm, 1000.0f64),
            (Attn::Idle, 500.0),
            (Attn::Static, 1000.0 / 3.0),
        ] {
            let c = Curve::quake(attn, 0x3fff);
            for d in (0..1500).step_by(7) {
                let scale = (1.0 - d as f64 / clip).max(0.0);
                let want = 16383.0 * scale;
                let got = c.level(d) as f64;
                assert!(
                    (got - want).abs() <= 24.0,
                    "{attn:?} at {d}: {got} vs {want}"
                );
            }
            assert_eq!(c.level(0), 0x3fff);
            assert_eq!(c.level(clip as i32 + 2), 0);
        }
    }

    #[test]
    fn goldsrc_attenuation_follows_vol_times_one_minus_dist_att_over_1000() {
        for att in [GOLDSRC_NORM, GOLDSRC_IDLE, GOLDSRC_STATIC, 300] {
            let c = Curve::goldsrc(0x3000, att);
            for d in (0..3500).step_by(11) {
                let scale = (1.0 - d as f64 * att as f64 / 1_000_000.0).max(0.0);
                let want = 12288.0 * scale;
                assert!((c.level(d) as f64 - want).abs() <= 20.0, "att {att} at {d}");
            }
        }
        assert_eq!(
            Curve::goldsrc(0x3000, 0).level(30000),
            0x3000,
            "ATTN_NONE is flat"
        );
    }

    #[test]
    fn the_inverse_law_halves_at_its_unit_and_never_rises() {
        let c = Curve::inverse(8000, 400);
        assert_eq!(c.level(0), 8000);
        assert_eq!(c.level(400), 4000);
        let mut last = i16::MAX;
        for d in 0..5000 {
            let v = c.level(d);
            assert!(v <= last);
            last = v;
        }
    }

    #[test]
    fn a_table_curve_interpolates_between_its_points_and_holds_past_the_end() {
        let mut pts = [0u16; 17];
        for (i, p) in pts.iter_mut().enumerate() {
            *p = 1600 - 100 * i as u16;
        }
        let c = Curve::Table { step_shift: 6, pts };
        assert_eq!(c.level(0), 1600);
        assert_eq!(c.level(64), 1500);
        assert_eq!(c.level(32), 1550);
        assert_eq!(c.level(64 * 16), 0);
        assert_eq!(c.level(100_000), 0);
    }

    #[test]
    fn an_empty_or_negative_clip_cannot_divide_by_zero() {
        // A clip under 256 units is held to the steepest slope: gone by 256.
        assert_eq!(Curve::linear_clip(1000, 0).level(300), 0);
        assert_eq!(Curve::linear_clip(1000, -5).level(0), 1000);
        assert_eq!(Curve::inverse(1000, 0).level(0), 1000);
    }

    #[test]
    fn a_sound_to_the_right_is_louder_in_the_right_ear_and_follows_the_heading() {
        let curve = Curve::Flat(8000);
        let g = place(PanLaw::Dot, [500, 0, 0], &curve).unwrap();
        assert!(g.right > g.left);
        assert_eq!((g.left, g.right), (0, 16000));
        // Dead ahead is centred; dead behind too.
        let g = place(PanLaw::Dot, [0, 0, 500], &curve).unwrap();
        assert_eq!(g.left, g.right);
        let g = place(PanLaw::Dot, [0, 0, -500], &curve).unwrap();
        assert_eq!(g.left, g.right);
        // Facing +x (a quarter turn right), the +x sound is dead ahead and the
        // +z sound is now on the left.
        let space = Space::new(0, PanLaw::Dot);
        let ls = Listeners::new([Listener::new([0; 3], 1024)]);
        let ahead = gains(
            &space,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([500, 0, 0]),
            &curve,
        )
        .unwrap();
        assert!(ahead.left.abs_diff(ahead.right) <= 4);
        let side = gains(
            &space,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([0, 0, 500]),
            &curve,
        )
        .unwrap();
        assert!(side.left > side.right);
    }

    #[test]
    fn a_sound_overhead_is_centred() {
        let g = place(PanLaw::Dot, [0, 800, 0], &Curve::Flat(8000)).unwrap();
        assert_eq!(g.left, g.right);
    }

    #[test]
    fn equal_power_keeps_the_sum_of_squares_constant_at_minus_3_db_centre() {
        let level = 10000i32;
        for pan in (-4096..=4096).step_by(128) {
            let (l, r) = PanLaw::EqualPower(Center::Minus3Db).apply(level, pan);
            let power = (l as f64).powi(2) + (r as f64).powi(2);
            let want = (level as f64).powi(2);
            assert!(
                (power / want - 1.0).abs() < 0.02,
                "pan {pan}: {power} vs {want}"
            );
        }
        let (l, r) = PanLaw::EqualPower(Center::Minus3Db).apply(level, 0);
        assert_eq!(l, r);
        assert!((l as f64 / level as f64 - core::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn equal_power_at_unity_centre_loses_nothing_for_a_centred_sound() {
        let law = PanLaw::EqualPower(Center::Unity);
        assert_eq!(law.apply(9000, 0), (9000, 9000));
        let (l, r) = law.apply(9000, -4096);
        assert_eq!((l, r), (9000, 0));
        let (l, r) = law.apply(9000, 4096);
        assert_eq!((l, r), (0, 9000));
        // Monotone: moving right never makes the right ear quieter.
        let mut last = -1;
        for pan in (-4096..=4096).step_by(64) {
            let (_, r) = law.apply(9000, pan);
            assert!(r >= last);
            last = r;
        }
    }

    #[test]
    fn the_dot_law_gives_each_ear_one_minus_or_plus_pan() {
        assert_eq!(PanLaw::Dot.apply(1000, 0), (1000, 1000));
        assert_eq!(PanLaw::Dot.apply(1000, 4096), (0, 2000));
        assert_eq!(PanLaw::Dot.apply(1000, -2048), (1500, 500));
    }

    #[test]
    fn the_sum_law_is_wipeouts_retail_split_for_every_level_and_bearing() {
        // WipEout `panned`: delta = v * a >> 11, left v + delta, right v - delta,
        // with a the bearing (positive to the left). Here pan = -4a.
        for v in (0..=0x2fffi32).step_by(37) {
            for a in -1024..=1024i32 {
                let delta = v.wrapping_mul(a) >> 11;
                let want = (v + delta, v - delta);
                assert_eq!(PanLaw::Sum.apply(v, -4 * a), want, "v {v} a {a}");
            }
        }
    }

    #[test]
    fn the_sum_law_reproduces_wipeouts_golden_placement() {
        // audio_placement_tests.rs: a sound at [4096, 0, 8192] from the origin is
        // magnitude 1144 in the game's units, bearing -302, and places at
        // (2374, 3195) after the sample-level scale 8190 >> 15.
        let v = 0x2fff - 1144;
        let (l, r) = PanLaw::Sum.apply(v, -4 * -302);
        assert_eq!(((l * 8190) >> 15, (r * 8190) >> 15), (2374, 3195));
    }

    #[test]
    fn a_custom_law_is_called_with_the_level_and_clamped_pan() {
        fn swap(level: i32, pan: i32) -> (i32, i32) {
            (pan, level)
        }
        assert_eq!(PanLaw::Custom(swap).apply(7, 99999), (4096, 7));
    }

    #[test]
    fn output_levels_are_clamped_to_the_spu_range() {
        let g = place(PanLaw::Sum, [0, 0, 100], &Curve::Flat(0x3fff)).unwrap();
        assert_eq!((g.left, g.right), (0x3fff, 0x3fff));
        fn inverted(level: i32, _: i32) -> (i32, i32) {
            (-level * 4, level * 4)
        }
        let g = place(PanLaw::Custom(inverted), [0, 0, 100], &Curve::Flat(0x3fff)).unwrap();
        assert_eq!((g.left, g.right), (-0x4000, 0x3fff));
    }

    #[test]
    fn a_sound_at_or_below_the_cull_level_is_inaudible_before_any_pan_work() {
        let space = Space::new(0, PanLaw::Dot).with_cull(100);
        let ls = Listeners::new([Listener::new([0; 3], 0)]);
        let curve = Curve::linear_clip(0x3fff, 1000);
        let near = Emitter::new([0, 0, 900]); // level 1638
        let far = Emitter::new([0, 0, 990]); // level about 163
        let gone = Emitter::new([0, 0, 995]); // level about 82
        assert!(gains(&space, &ls, ListenerRule::Loudest, &near, &curve).is_some());
        assert!(gains(&space, &ls, ListenerRule::Loudest, &far, &curve).is_some());
        assert!(gains(&space, &ls, ListenerRule::Loudest, &gone, &curve).is_none());
    }

    fn two() -> (Space, Listeners<2>) {
        (
            Space::new(0, PanLaw::EqualPower(Center::Minus3Db)),
            Listeners::new([Listener::new([0, 0, 0], 0), Listener::new([4000, 0, 0], 0)]),
        )
    }

    #[test]
    fn split_screen_loudest_hears_a_sound_from_the_listener_it_is_louder_to() {
        let (sp, ls) = two();
        let curve = Curve::linear_clip(0x3fff, 6000);
        let by_p1 = gains(
            &sp,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([300, 0, 100]),
            &curve,
        )
        .unwrap();
        assert_eq!(by_p1.listener, 0);
        let by_p2 = gains(
            &sp,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([3700, 0, 100]),
            &curve,
        )
        .unwrap();
        assert_eq!(by_p2.listener, 1);
        // A tie goes to the first listener, as WipEout's strict `>` does.
        let mid = gains(
            &sp,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([2000, 0, 300]),
            &curve,
        )
        .unwrap();
        assert_eq!(mid.listener, 0);
    }

    #[test]
    fn nearest_listener_decides_even_when_the_other_one_would_hear_it_louder() {
        let (sp, ls) = two();
        // Close to listener 1 (dist 100) but on a flat curve both hear the same.
        let flat = Curve::Flat(5000);
        let g = gains(
            &sp,
            &ls,
            ListenerRule::Nearest,
            &Emitter::new([3900, 0, 0]),
            &flat,
        )
        .unwrap();
        assert_eq!(g.listener, 1);
        assert_eq!(g.dist, 100);
        let g = gains(
            &sp,
            &ls,
            ListenerRule::Nearest,
            &Emitter::new([100, 0, 0]),
            &flat,
        )
        .unwrap();
        assert_eq!(g.listener, 0);
        // A loop sitting between them keeps the same listener as it moves a
        // little, where Loudest on an uneven curve could flip.
        let mut last = None;
        for x in 2001..2010 {
            let g = gains(
                &sp,
                &ls,
                ListenerRule::Nearest,
                &Emitter::new([x, 0, 0]),
                &flat,
            )
            .unwrap();
            if let Some(prev) = last {
                assert_eq!(g.listener, prev);
            }
            last = Some(g.listener);
        }
        assert_eq!(last, Some(1));
    }

    #[test]
    fn centroid_hears_a_sound_from_the_mean_position_so_neither_player_is_privileged() {
        let (sp, ls) = two();
        let curve = Curve::linear_clip(0x3fff, 6000);
        // Straight ahead of the midpoint (2000, 0, 0): centred and 1000 away.
        let g = gains(
            &sp,
            &ls,
            ListenerRule::Centroid,
            &Emitter::new([2000, 0, 1000]),
            &curve,
        )
        .unwrap();
        assert_eq!(g.dist, 1000);
        assert!(g.left.abs_diff(g.right) <= 8, "{g:?}");
        // Loudest, by contrast, picks a side.
        let l = gains(
            &sp,
            &ls,
            ListenerRule::Loudest,
            &Emitter::new([2000, 0, 1000]),
            &curve,
        )
        .unwrap();
        assert!(l.dist > 1000);
    }

    #[test]
    fn a_scene_with_no_listener_hears_nothing() {
        let sp = Space::new(0, PanLaw::Off);
        let none = Listeners::<0>::new([]);
        assert!(none.is_empty());
        assert!(gains(
            &sp,
            &none,
            ListenerRule::Nearest,
            &Emitter::new([0; 3]),
            &Curve::Flat(9)
        )
        .is_none());
    }

    #[test]
    fn audibility_is_the_louder_ear_floored_at_zero() {
        let g = Gains {
            left: -500,
            right: -100,
            listener: 0,
            dist: 0,
        };
        assert_eq!(g.audibility(), 0);
        let g = Gains {
            left: 300,
            right: 900,
            listener: 0,
            dist: 0,
        };
        assert_eq!(g.audibility(), 900);
    }
}
