//! Voice lanes, priorities, stealing, and sound handles.
//!
//! Ten programs on the demo disc each own a private way of sharing the SPU's 24
//! voices: a voice per job, a round robin of four, a rotating pool of fifteen,
//! first-free with strict priority. [`Mixer`] is the one version. The 24 voices
//! are split once, at construction, into [`Lane`]s; each lane carries a
//! [`Policy`] that decides what a new sound does when its lane is busy.
//!
//! A [`Mixer`] is bookkeeping only. It never touches a register; every decision
//! goes out through [`Hw`], which on the host is a [`Recorder`](crate::hw::Recorder).
//!
//! # Time
//!
//! The mixer has a clock it does not own: call [`Mixer::tick`] once per frame
//! (or per whatever unit the game's sound durations are in) with a counter that
//! only goes up. One-shot lifetimes, release tails and the wrap of that counter
//! are all handled against it. An idle mixer pays two compares per tick.
//!
//! # Example
//!
//! ```
//! use psx_sfx::hw::Recorder;
//! use psx_sfx::mixer::*;
//! use psx_sfx::Sample;
//! use psx_spu::SpuAddr;
//!
//! // UI sounds get two voices, round robin; the world gets the rest.
//! let mut mixer = Mixer::new(
//!     [
//!         Lane::new(0x00_0003, Policy::RoundRobin),
//!         Lane::new(0xFF_FFFC, Policy::Smart),
//!     ],
//!     60,
//! );
//! let mut hw = Recorder::<32>::new();
//!
//! let beep = Sound::one_shot(SoundId(1), Sample::resident(SpuAddr::new(0x1010), 22050, 40), 0);
//! mixer.tick(&mut hw, 0);
//! let h = mixer.play(&mut hw, &beep, Play::new(0x2000, 0x2000)).unwrap();
//! assert!(mixer.is_alive(h));
//!
//! // 40 blocks at 22.05 kHz is 3 ticks of sound plus the 2 tick margin.
//! mixer.tick(&mut hw, 4);
//! assert!(mixer.is_alive(h));
//! mixer.tick(&mut hw, 5);
//! assert!(!mixer.is_alive(h));
//! ```

use crate::hw::Hw;
use crate::master::Master;
use crate::{expired, Sample};
use psx_spu::Pitch;

/// Voices on the SPU.
pub const VOICES: usize = 24;

/// Mask of all 24 voices.
pub const ALL_VOICES: u32 = 0x00FF_FFFF;

/// Ticks a keyed-off voice is considered still ringing before the mixer treats
/// it as free again.
pub const RELEASE_TICKS: u32 = 2;

/// A playing one-shot with this many ticks or fewer left counts as finishing,
/// and is taken before a victim is chosen.
const FINISHING_TICKS: u32 = 2;

/// Unity pitch multiplier.
const PITCH_UNITY: u16 = 4096;

/// How the voices in a lane are shared out.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::{Lane, Policy};
///
/// let lane = Lane::new(0x00_00F0, Policy::FirstFree);
/// assert_eq!(lane.policy(), Policy::FirstFree);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Policy {
    /// Take the next voice in the lane after the last one used, whether it is
    /// idle or not. Cheap; a new sound cuts the oldest. Protected sounds are
    /// skipped.
    RoundRobin,
    /// Take the lowest free voice; with none free, drop the new sound. Never
    /// steals, so it suits lanes of loops that something else supervises.
    FirstFree,
    /// The default for new code: a free voice, else one that is finishing or
    /// ringing, else the least important live sound, else drop. See
    /// [`Mixer::play`] for the exact order.
    Smart,
    /// Allocation exactly as WipEout's retail sound driver makes it: the first
    /// idle voice in the lane, else the first finite sound with a strictly
    /// lower priority (a larger number); loops are never taken and an equal
    /// priority cannot steal. Expiry ends a sound at once with no release tail,
    /// and the playing sound is keyed off without being silenced first.
    StrictRetail,
}

/// A set of voices and the policy that shares them.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::{Lane, Policy};
///
/// // Voices 0..=1, round robin.
/// let ui = Lane::new(0b11, Policy::RoundRobin);
/// assert_eq!(ui.voices(), 0b11);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    voices: u32,
    policy: Policy,
}

impl Lane {
    /// A lane over the voices set in `voices` (bit `n` is voice `n`).
    pub const fn new(voices: u32, policy: Policy) -> Self {
        Self { voices, policy }
    }

    /// The voice mask.
    pub const fn voices(&self) -> u32 {
        self.voices
    }

    /// The lane's policy.
    pub const fn policy(&self) -> Policy {
        self.policy
    }
}

/// How much a sound matters, most important first.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::Class;
///
/// assert!(Class::Protected < Class::High);
/// assert!(Class::High < Class::Low);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Never stolen, by anything, and never dropped for being quiet: a goal
    /// horn, a whistle, dialogue. If every voice a protected sound could use
    /// is protected too, the new one is dropped instead.
    Protected,
    /// Gameplay-critical effects.
    High,
    /// Ordinary effects.
    Normal,
    /// Ambient and decorative sounds, first to go.
    Low,
}

/// A sound's identity in the caller's tables. The mixer carries it through and
/// never interprets it.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::SoundId;
///
/// const HIT: SoundId = SoundId(3);
/// assert_eq!(HIT.0, 3);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundId(pub u16);

/// A sample together with how the mixer should treat it.
///
/// Within a class a smaller `prio` is more important, matching WipEout's signed
/// priority and Quake's channel order.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::*;
/// use psx_sfx::Sample;
/// use psx_spu::SpuAddr;
///
/// let s = Sample::resident(SpuAddr::new(0x1010), 22050, 40);
/// let horn = Sound::one_shot(SoundId(9), s, 1).with_class(Class::Protected).with_volume(0x3000);
/// assert_eq!(horn.class(), Class::Protected);
/// assert!(!horn.is_looped());
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Sound {
    id: SoundId,
    sample: Sample,
    lane: u8,
    class: Class,
    prio: i16,
    looped: bool,
    volume: i16,
    pitch_q12: u16,
    duck_q12: u16,
}

impl Sound {
    const fn base(id: SoundId, sample: Sample, lane: u8, looped: bool) -> Self {
        Self {
            id,
            sample,
            lane,
            class: Class::Normal,
            prio: 0,
            looped,
            volume: 0x3FFF,
            pitch_q12: PITCH_UNITY,
            duck_q12: 4096,
        }
    }

    /// A sample that plays once and ends, on lane `lane`.
    pub const fn one_shot(id: SoundId, sample: Sample, lane: u8) -> Self {
        Self::base(id, sample, lane, false)
    }

    /// A sample whose cooked blocks form a hardware loop, on lane `lane`. It
    /// plays until stopped.
    pub const fn looped(id: SoundId, sample: Sample, lane: u8) -> Self {
        Self::base(id, sample, lane, true)
    }

    /// Set the importance class.
    pub const fn with_class(mut self, class: Class) -> Self {
        self.class = class;
        self
    }

    /// Set the priority within the class; smaller is more important.
    pub const fn with_prio(mut self, prio: i16) -> Self {
        self.prio = prio;
        self
    }

    /// Set the default per-ear level used by [`Play::for_sound`].
    pub const fn with_volume(mut self, volume: i16) -> Self {
        self.volume = volume;
        self
    }

    /// Set the default pitch multiplier (`4096` = as cooked).
    pub const fn with_pitch(mut self, pitch_q12: u16) -> Self {
        self.pitch_q12 = pitch_q12;
        self
    }

    /// While this sound plays, ask for the music to be ducked to `gain_q12`
    /// (`4096` = no duck, `2048` = half).
    pub const fn with_duck(mut self, gain_q12: u16) -> Self {
        self.duck_q12 = gain_q12;
        self
    }

    /// The caller's identity for this sound.
    pub const fn id(&self) -> SoundId {
        self.id
    }

    /// The sample.
    pub const fn sample(&self) -> Sample {
        self.sample
    }

    /// The lane this sound plays on.
    pub const fn lane(&self) -> u8 {
        self.lane
    }

    /// The importance class.
    pub const fn class(&self) -> Class {
        self.class
    }

    /// Priority within the class.
    pub const fn prio(&self) -> i16 {
        self.prio
    }

    /// Does it loop until stopped?
    pub const fn is_looped(&self) -> bool {
        self.looped
    }
}

/// How one play request should be voiced.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::Play;
///
/// let p = Play::new(0x2000, 0x1000).with_key(7).with_ticks(30);
/// assert_eq!((p.left(), p.right()), (0x2000, 0x1000));
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Play {
    left: i16,
    right: i16,
    pitch_q12: u16,
    key: u16,
    ticks: u32,
}

impl Play {
    /// Play at these per-ear levels (signed SPU range, before the master
    /// volume), at the sound's own pitch, replacing nothing, lasting as long as
    /// its sample.
    pub const fn new(left: i16, right: i16) -> Self {
        Self {
            left,
            right,
            pitch_q12: 0,
            key: 0,
            ticks: 0,
        }
    }

    /// Play at `sound`'s default level on both ears.
    pub const fn for_sound(sound: &Sound) -> Self {
        Self::new(sound.volume, sound.volume)
    }

    /// Override the pitch multiplier for this play (`0` keeps the sound's own).
    pub const fn with_pitch(mut self, pitch_q12: u16) -> Self {
        self.pitch_q12 = pitch_q12;
        self
    }

    /// Replace in place: if a sound with the same non-zero `key` is already
    /// playing in the lane, this one takes its voice instead of a second one.
    /// Quake's (owner, channel) rule.
    pub const fn with_key(mut self, key: u16) -> Self {
        self.key = key;
        self
    }

    /// Length in ticks, overriding the one computed from the sample's block
    /// count. Needed for samples with no recorded length, and for games that
    /// time a sound in their own steps.
    pub const fn with_ticks(mut self, ticks: u32) -> Self {
        self.ticks = ticks;
        self
    }

    /// Left level.
    pub const fn left(&self) -> i16 {
        self.left
    }

    /// Right level.
    pub const fn right(&self) -> i16 {
        self.right
    }
}

/// A claim on one playing sound, safe to hold across frames.
///
/// It names a voice and a generation. Once the sound ends or its voice is
/// stolen the generation moves on and every call with the old handle does
/// nothing, so an engine loop can keep its handle without checking whether it
/// still owns the voice. The generation is 11 bits; a stale handle could alias a
/// live one only after 2048 reallocations of the same voice.
///
/// # Example
///
/// ```
/// use psx_sfx::mixer::Handle;
///
/// let h = Handle::from_raw(0b00001_00000_00101);
/// assert_eq!(h.voice(), 5);
/// assert_eq!(Handle::from_raw(h.raw()), h);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(u16);

impl Handle {
    const fn pack(voice: usize, generation: u16) -> Self {
        Self((generation << 5) | voice as u16)
    }

    /// Rebuild a handle from [`raw`](Self::raw), for a game that stores them in
    /// a compact table.
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// The packed value: voice in the low 5 bits, generation above.
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// The voice number this handle names.
    pub const fn voice(self) -> u8 {
        (self.0 & 31) as u8
    }

    /// The generation, 11 bits.
    pub const fn generation(self) -> u16 {
        self.0 >> 5
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Playing,
    Releasing,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    state: State,
    generation: u16,
    looped: bool,
    retail: bool,
    has_deadline: bool,
    class: Class,
    prio: i16,
    key: u16,
    addr: u32,
    rate_hz: u32,
    off_at: u32,
    left: i16,
    right: i16,
    duck_q12: u16,
}

impl Slot {
    const IDLE: Slot = Slot {
        state: State::Idle,
        generation: 0,
        looped: false,
        retail: false,
        has_deadline: false,
        class: Class::Normal,
        prio: 0,
        key: 0,
        addr: 0,
        rate_hz: 0,
        off_at: 0,
        left: 0,
        right: 0,
        duck_q12: 4096,
    };

    fn rank(&self) -> i32 {
        rank(self.class, self.prio)
    }

    fn loudness(&self) -> i32 {
        self.left.max(self.right) as i32
    }

    fn remaining(&self, now: u32) -> u32 {
        if self.looped || !self.has_deadline {
            u32::MAX
        } else if expired(now, self.off_at) {
            0
        } else {
            self.off_at.wrapping_sub(now)
        }
    }
}

/// Importance as a single number, smaller is more important: class first, then
/// priority.
const fn rank(class: Class, prio: i16) -> i32 {
    (class as i32) * 65536 + prio as i32 + 32768
}

/// Is `a` strictly earlier than `b` on a wrapping counter?
const fn before(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < u32::MAX / 2
}

/// The pitch multiplier the hardware can actually reach for a sample cooked at
/// `rate_hz`: the SPU's pitch register tops out at 0x3FFF, which is 4x the 44.1
/// kHz base.
fn pitch_cap_q12(rate_hz: u32) -> u32 {
    let rate = if rate_hz == 0 { 44_100 } else { rate_hz };
    ((Pitch::MAX.raw_value() as u32 * 44_100) / rate).min(u16::MAX as u32)
}

/// The voice allocator: lanes, policies, handles and lifetimes over the SPU's 24
/// voices, for `L` lanes.
///
/// # Example
///
/// ```
/// use psx_sfx::hw::{Event, Recorder};
/// use psx_sfx::mixer::*;
/// use psx_sfx::Sample;
/// use psx_spu::SpuAddr;
///
/// let mut mixer = Mixer::new([Lane::new(0b1, Policy::Smart)], 60);
/// let mut hw = Recorder::<16>::new();
/// let hum = Sound::looped(SoundId(2), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
///
/// let h = mixer.play(&mut hw, &hum, Play::new(1000, 1000)).unwrap();
/// mixer.update(&mut hw, h, 2000, 500);
/// assert_eq!(hw.last_volume(0), Some((2000, 500)));
///
/// mixer.stop(&mut hw, h);
/// assert!(!mixer.is_alive(h));
/// assert!(!mixer.update(&mut hw, h, 1, 1)); // a stale handle is a no-op
/// assert_eq!(hw.keyed_off(), 1);
/// ```
#[derive(Clone, Debug)]
pub struct Mixer<const L: usize> {
    lanes: [Lane; L],
    slots: [Slot; VOICES],
    cursor: [u8; L],
    pending: u32,
    next_due: u32,
    ducking: u32,
    now: u32,
    ticks_hz: u32,
    cull: i16,
    master: Master,
}

impl<const L: usize> Mixer<L> {
    /// A mixer over `lanes`, with durations measured against a clock that runs
    /// at `ticks_hz` (60 for NTSC frames, 50 for PAL).
    ///
    /// Lanes must lie inside the 24 voices and must not share a voice. Built in
    /// a `const`, a violation is a compile error.
    ///
    /// # Panics
    /// If a lane is empty, uses a voice above 23, or overlaps another lane.
    pub const fn new(lanes: [Lane; L], ticks_hz: u32) -> Self {
        let mut seen = 0u32;
        let mut i = 0;
        while i < L {
            let m = lanes[i].voices;
            assert!(m != 0, "a lane needs at least one voice");
            assert!(m & !ALL_VOICES == 0, "a lane uses a voice above 23");
            assert!(m & seen == 0, "lanes must not share a voice");
            seen |= m;
            i += 1;
        }
        Self {
            lanes,
            slots: [Slot::IDLE; VOICES],
            cursor: [0; L],
            pending: 0,
            next_due: 0,
            ducking: 0,
            now: 0,
            ticks_hz,
            cull: 0,
            master: Master::new(),
        }
    }

    /// Sounds whose louder ear is at or below `cull` do not take a voice (unless
    /// protected). Default `0`.
    ///
    /// ```
    /// # use psx_sfx::mixer::*;
    /// let mut mixer = Mixer::new([Lane::new(1, Policy::Smart)], 60);
    /// mixer.set_cull(100);
    /// assert_eq!(mixer.cull(), 100);
    /// ```
    pub fn set_cull(&mut self, cull: i16) {
        self.cull = cull;
    }

    /// The cull threshold.
    pub const fn cull(&self) -> i16 {
        self.cull
    }

    /// The master volumes and duck.
    pub const fn master(&self) -> &Master {
        &self.master
    }

    /// Replace the master volumes and re-apply them to every live voice at
    /// once, from the unscaled levels the mixer kept.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// use psx_sfx::master::{Master, Ratio};
    /// let mut half = Master::new();
    /// half.set_sfx(Ratio::new(1, 2));
    /// mixer.set_master(&mut hw, half);
    /// assert_eq!(hw.last_volume(h.voice()), Some((500, 500)));
    /// ```
    pub fn set_master<H: Hw>(&mut self, hw: &mut H, master: Master) {
        self.master = master;
        for v in 0..VOICES {
            if self.slots[v].state == State::Playing {
                self.write_levels(hw, v);
            }
        }
    }

    /// Advance the clock to `now` and retire what has finished.
    ///
    /// Call once per frame before anything that might start a sound. When no
    /// sound is due and no duck is moving this is two compares.
    ///
    /// ```
    /// # use psx_sfx::{hw::NullHw, mixer::*};
    /// let mut mixer = Mixer::new([Lane::new(1, Policy::Smart)], 60);
    /// for frame in 0..3 {
    ///     mixer.tick(&mut NullHw, frame);
    /// }
    /// ```
    pub fn tick<H: Hw>(&mut self, hw: &mut H, now: u32) {
        self.now = now;
        if self.pending != 0 && expired(now, self.next_due) {
            self.retire(hw);
        }
        if self.ducking != 0 || !self.master.duck_settled(4096) {
            let mut target = 4096u16;
            for v in 0..VOICES {
                if self.ducking & (1 << v) != 0 {
                    target = target.min(self.slots[v].duck_q12);
                }
            }
            self.master.step_duck(target);
        }
    }

    fn retire<H: Hw>(&mut self, hw: &mut H) {
        let now = self.now;
        let mut off = 0u32;
        let mut first: Option<u32> = None;
        for v in 0..VOICES {
            let bit = 1u32 << v;
            if self.pending & bit == 0 {
                continue;
            }
            let s = &mut self.slots[v];
            if expired(now, s.off_at) {
                match s.state {
                    State::Playing => {
                        off |= bit;
                        self.ducking &= !bit;
                        if s.retail {
                            s.state = State::Idle;
                            self.pending &= !bit;
                        } else {
                            hw.set_volume(v as u8, 0, 0);
                            s.state = State::Releasing;
                            s.off_at = now.wrapping_add(RELEASE_TICKS);
                            first = Some(Self::earlier(first, s.off_at));
                        }
                    }
                    State::Releasing | State::Idle => {
                        s.state = State::Idle;
                        self.pending &= !bit;
                    }
                }
            } else {
                first = Some(Self::earlier(first, s.off_at));
            }
        }
        if let Some(at) = first {
            self.next_due = at;
        }
        if off != 0 {
            hw.key_off(off);
        }
    }

    fn earlier(a: Option<u32>, b: u32) -> u32 {
        match a {
            Some(a) if !before(b, a) => a,
            _ => b,
        }
    }

    fn arm(&mut self, v: usize, at: u32) {
        self.slots[v].off_at = at;
        if self.pending == 0 || before(at, self.next_due) {
            self.next_due = at;
        }
        self.pending |= 1 << v;
    }

    fn write_levels<H: Hw>(&self, hw: &mut H, v: usize) {
        let s = &self.slots[v];
        hw.set_volume(
            v as u8,
            self.master.scale_level(s.left),
            self.master.scale_level(s.right),
        );
    }

    fn lane_of(&self, sound: &Sound) -> Option<(usize, Lane)> {
        let i = sound.lane as usize;
        self.lanes.get(i).map(|l| (i, *l))
    }

    /// Start `sound`, or decide not to.
    ///
    /// Returns a [`Handle`] for the new sound, or `None` when it was culled or
    /// the lane had nothing to give. The order in which a lane under
    /// [`Policy::Smart`] finds a voice:
    ///
    /// 1. The same non-zero [`Play::with_key`] already playing in the lane: take
    ///    its voice.
    /// 2. An idle voice.
    /// 3. A voice that is only ringing from a key-off, or a one-shot with a
    ///    tick or two left.
    /// 4. The least important live sound that is not protected and is no more
    ///    important than the new one: lowest importance first, then quietest,
    ///    then least time left. Equal importance may steal, but not a loop, and
    ///    not a louder sound.
    /// 5. Otherwise the new sound is dropped. A protected sound is never taken.
    ///
    /// Sounds that are inaudible (see [`set_cull`](Self::set_cull)) are
    /// dropped before any of this, unless protected.
    pub fn play<H: Hw>(&mut self, hw: &mut H, sound: &Sound, play: Play) -> Option<Handle> {
        let (lane_index, lane) = self.lane_of(sound)?;
        let loud = play.left.max(play.right);
        if sound.class != Class::Protected && loud <= self.cull {
            return None;
        }
        let v = self.pick(lane_index, lane, sound, play.key, loud as i32)?;

        let rate = sound.sample.sample_rate_hz();
        let requested = u32::from(if play.pitch_q12 != 0 {
            play.pitch_q12
        } else if sound.pitch_q12 != 0 {
            sound.pitch_q12
        } else {
            PITCH_UNITY
        });
        let pitch = requested.clamp(1, pitch_cap_q12(rate).max(1)) as u16;

        let deadline = if sound.looped {
            None
        } else if play.ticks != 0 {
            Some(play.ticks)
        } else {
            sound.sample.ticks_at(self.ticks_hz).map(|base| {
                // Half the pitch is twice the duration: stretch the cutoff.
                base.saturating_mul(PITCH_UNITY as u32) / pitch as u32
            })
        };

        let s = &mut self.slots[v];
        s.generation = (s.generation + 1) & 0x7FF;
        s.state = State::Playing;
        s.looped = sound.looped;
        s.retail = lane.policy == Policy::StrictRetail;
        s.has_deadline = deadline.is_some();
        s.class = sound.class;
        s.prio = sound.prio;
        s.key = play.key;
        s.addr = sound.sample.addr().byte_offset();
        s.rate_hz = rate;
        s.left = play.left;
        s.right = play.right;
        s.duck_q12 = sound.duck_q12;
        let generation = s.generation;
        let bit = 1u32 << v;
        match deadline {
            Some(t) => {
                let at = self.now.wrapping_add(t);
                self.arm(v, at);
            }
            None => self.pending &= !bit,
        }
        if sound.duck_q12 < 4096 {
            self.ducking |= bit;
        } else {
            self.ducking &= !bit;
        }

        hw.configure(v as u8, &sound.sample, sound.looped, pitch);
        self.write_levels(hw, v);
        hw.key_on(bit);
        Some(Handle::pack(v, generation))
    }

    fn pick(
        &mut self,
        lane_index: usize,
        lane: Lane,
        sound: &Sound,
        key: u16,
        loud: i32,
    ) -> Option<usize> {
        let mask = lane.voices;
        if key != 0 {
            for v in 0..VOICES {
                let s = &self.slots[v];
                if mask & (1 << v) != 0 && s.state == State::Playing && s.key == key {
                    return Some(v);
                }
            }
        }
        match lane.policy {
            Policy::RoundRobin => self.pick_round_robin(lane_index, mask),
            Policy::FirstFree => self.pick_first_free(mask),
            Policy::Smart => self.pick_smart(mask, rank(sound.class, sound.prio), loud),
            Policy::StrictRetail => self.pick_strict(mask, rank(sound.class, sound.prio)),
        }
    }

    fn pick_round_robin(&mut self, lane_index: usize, mask: u32) -> Option<usize> {
        let start = self.cursor[lane_index] as usize;
        for k in 1..=VOICES {
            let v = (start + k) % VOICES;
            if mask & (1 << v) == 0 {
                continue;
            }
            let s = &self.slots[v];
            if s.state == State::Playing && s.class == Class::Protected {
                continue;
            }
            self.cursor[lane_index] = v as u8;
            return Some(v);
        }
        None
    }

    fn pick_first_free(&self, mask: u32) -> Option<usize> {
        let mut ringing = None;
        for v in 0..VOICES {
            if mask & (1 << v) == 0 {
                continue;
            }
            match self.slots[v].state {
                State::Idle => return Some(v),
                State::Releasing if ringing.is_none() => ringing = Some(v),
                _ => {}
            }
        }
        ringing
    }

    fn pick_strict(&self, mask: u32, new_rank: i32) -> Option<usize> {
        let mut steal = None;
        for v in 0..VOICES {
            if mask & (1 << v) == 0 {
                continue;
            }
            let s = &self.slots[v];
            match s.state {
                State::Idle => return Some(v),
                State::Playing
                    if steal.is_none()
                        && !s.looped
                        && s.class != Class::Protected
                        && new_rank < s.rank() =>
                {
                    steal = Some(v);
                }
                _ => {}
            }
        }
        steal
    }

    fn pick_smart(&self, mask: u32, new_rank: i32, new_loud: i32) -> Option<usize> {
        let mut ringing = None;
        let mut finishing = None;
        // (voice, rank, loudness, remaining)
        let mut victim: Option<(usize, i32, i32, u32)> = None;
        for v in 0..VOICES {
            if mask & (1 << v) == 0 {
                continue;
            }
            let s = &self.slots[v];
            match s.state {
                State::Idle => return Some(v),
                State::Releasing => {
                    if ringing.is_none() {
                        ringing = Some(v);
                    }
                }
                State::Playing => {
                    if s.class == Class::Protected {
                        continue;
                    }
                    let rem = s.remaining(self.now);
                    if rem <= FINISHING_TICKS && finishing.is_none() {
                        finishing = Some(v);
                    }
                    let r = s.rank();
                    if r < new_rank {
                        continue; // more important than the newcomer
                    }
                    let loud = s.loudness();
                    if r == new_rank && (s.looped || new_loud < loud) {
                        continue; // never trade a loud sound for a quiet one
                    }
                    let better = match victim {
                        None => true,
                        Some((_, br, bl, brem)) => {
                            r > br || (r == br && (loud < bl || (loud == bl && rem < brem)))
                        }
                    };
                    if better {
                        victim = Some((v, r, loud, rem));
                    }
                }
            }
        }
        ringing.or(finishing).or(victim.map(|(v, ..)| v))
    }

    /// Is `handle` still the sound it was issued for?
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert!(mixer.is_alive(h));
    /// mixer.stop(&mut hw, h);
    /// assert!(!mixer.is_alive(h));
    /// ```
    pub fn is_alive(&self, handle: Handle) -> bool {
        let s = &self.slots[handle.voice() as usize % VOICES];
        s.state == State::Playing && s.generation == handle.generation()
    }

    /// Change a live sound's levels (before the master volume). Writes nothing
    /// when the levels are unchanged. Returns `false` and does nothing for a
    /// stale handle.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert!(mixer.update(&mut hw, h, 3000, 100));
    /// assert_eq!(hw.last_volume(h.voice()), Some((3000, 100)));
    /// ```
    pub fn update<H: Hw>(&mut self, hw: &mut H, handle: Handle, left: i16, right: i16) -> bool {
        if !self.is_alive(handle) {
            return false;
        }
        let v = handle.voice() as usize;
        let s = &mut self.slots[v];
        if s.left != left || s.right != right {
            s.left = left;
            s.right = right;
            self.write_levels(hw, v);
        }
        true
    }

    /// Change a live sound's pitch multiplier (`4096` = as cooked), clamped to
    /// what the hardware can reach. Returns `false` for a stale handle. The
    /// sound's cutoff is not recomputed; use it for loops.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert!(mixer.set_pitch(&mut hw, h, 6144)); // a fifth up
    /// ```
    pub fn set_pitch<H: Hw>(&mut self, hw: &mut H, handle: Handle, pitch_q12: u16) -> bool {
        if !self.is_alive(handle) {
            return false;
        }
        let rate_hz = self.slots[handle.voice() as usize].rate_hz;
        let pitch = (pitch_q12 as u32).clamp(1, pitch_cap_q12(rate_hz).max(1)) as u16;
        hw.set_pitch(handle.voice(), pitch);
        true
    }

    /// End a live sound `ticks` ticks from now, whatever it was going to do. A
    /// retail-style "kill with a release". Returns `false` for a stale handle.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// mixer.tick(&mut hw, 0);
    /// mixer.end_in(h, 8);
    /// mixer.tick(&mut hw, 7);
    /// assert!(mixer.is_alive(h));
    /// mixer.tick(&mut hw, 8);
    /// assert!(!mixer.is_alive(h));
    /// ```
    pub fn end_in(&mut self, handle: Handle, ticks: u32) -> bool {
        if !self.is_alive(handle) {
            return false;
        }
        let v = handle.voice() as usize;
        self.slots[v].looped = false;
        self.slots[v].has_deadline = true;
        let at = self.now.wrapping_add(ticks);
        self.arm(v, at);
        true
    }

    /// Silence a live sound and release its voice. Returns `false` for a stale
    /// handle.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert!(mixer.stop(&mut hw, h));
    /// assert!(!mixer.stop(&mut hw, h)); // already gone
    /// ```
    pub fn stop<H: Hw>(&mut self, hw: &mut H, handle: Handle) -> bool {
        if !self.is_alive(handle) {
            return false;
        }
        self.silence(hw, handle.voice() as usize);
        true
    }

    fn silence<H: Hw>(&mut self, hw: &mut H, v: usize) {
        let bit = 1u32 << v;
        let s = &mut self.slots[v];
        self.ducking &= !bit;
        if s.retail {
            s.state = State::Idle;
            self.pending &= !bit;
        } else {
            hw.set_volume(v as u8, 0, 0);
            s.state = State::Releasing;
            let at = self.now.wrapping_add(RELEASE_TICKS);
            self.arm(v, at);
        }
        hw.key_off(bit);
    }

    /// Stop every playing sound.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// mixer.stop_all(&mut hw);
    /// assert_eq!(mixer.playing(), 0);
    /// ```
    pub fn stop_all<H: Hw>(&mut self, hw: &mut H) {
        self.stop_where(hw, |_| true);
    }

    /// Stop every sound whose sample starts in `lo..hi` (SPU RAM byte offsets).
    /// Call before a DMA over samples that may be playing.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// mixer.stop_range(&mut hw, 0x2000, 0x3000); // the sound starts at 0x2000
    /// assert!(!mixer.is_alive(h));
    /// ```
    pub fn stop_range<H: Hw>(&mut self, hw: &mut H, lo: u32, hi: u32) {
        self.stop_where(hw, |s| s.addr >= lo && s.addr < hi);
    }

    fn stop_where<H: Hw>(&mut self, hw: &mut H, hit: impl Fn(&Slot) -> bool) {
        for v in 0..VOICES {
            if self.slots[v].state == State::Playing && hit(&self.slots[v]) {
                self.silence(hw, v);
            }
        }
    }

    /// The voices holding a live sound, as a mask.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert_eq!(mixer.playing(), 1 << h.voice());
    /// ```
    pub fn playing(&self) -> u32 {
        let mut m = 0;
        for (v, s) in self.slots.iter().enumerate() {
            if s.state == State::Playing {
                m |= 1 << v;
            }
        }
        m
    }

    /// The voices of `lane` that could take a sound without stealing: idle, or
    /// only ringing from a key-off.
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert_eq!(mixer.free_in_lane(0), 0b11 & !(1 << h.voice()));
    /// ```
    pub fn free_in_lane(&self, lane: usize) -> u32 {
        let Some(l) = self.lanes.get(lane) else {
            return 0;
        };
        let mut m = 0;
        for (v, s) in self.slots.iter().enumerate() {
            if l.voices & (1 << v) != 0 && s.state != State::Playing {
                m |= 1 << v;
            }
        }
        m
    }

    /// Is any voice of `lane` free? See [`free_in_lane`](Self::free_in_lane).
    ///
    /// ```
    /// # use psx_sfx::{hw::Recorder, mixer::*, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(0b11, Policy::Smart)], 60);
    /// # let mut hw = Recorder::<16>::new();
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// # let h = mixer.play(&mut hw, &sound, Play::new(1000, 1000)).unwrap();
    /// assert!(mixer.has_free(0));
    /// assert!(!mixer.has_free(7)); // no such lane
    /// ```
    pub fn has_free(&self, lane: usize) -> bool {
        self.free_in_lane(lane) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::{Event, Recorder};
    use crate::master::Ratio;
    use psx_spu::SpuAddr;

    type Rec = Recorder<256>;

    fn sample() -> Sample {
        // 400 blocks at 22.05 kHz: 30 ticks of sound plus the 2 tick margin.
        Sample::resident(SpuAddr::new(0x1010), 22050, 400)
    }

    fn shot(class: Class, prio: i16) -> Sound {
        Sound::one_shot(SoundId(1), sample(), 0)
            .with_class(class)
            .with_prio(prio)
    }

    fn looped(class: Class, prio: i16) -> Sound {
        Sound::looped(SoundId(2), sample(), 0)
            .with_class(class)
            .with_prio(prio)
    }

    fn smart(voices: u32) -> Mixer<1> {
        Mixer::new([Lane::new(voices, Policy::Smart)], 60)
    }

    fn at(level: i16) -> Play {
        Play::new(level, level)
    }

    #[test]
    fn a_free_voice_is_taken_before_anything_is_stolen() {
        let mut m = smart(0b111);
        let mut hw = Rec::new();
        let a = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
        let b = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
        let c = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
        let mut voices = [a.voice(), b.voice(), c.voice()];
        voices.sort_unstable();
        assert_eq!(voices, [0, 1, 2]);
        assert!(m.is_alive(a) && m.is_alive(b) && m.is_alive(c));
    }

    #[test]
    fn a_protected_sound_is_never_stolen_by_anything() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        let p1 = m.play(&mut hw, &shot(Class::Protected, 0), at(10)).unwrap();
        let p2 = m.play(&mut hw, &shot(Class::Protected, 0), at(10)).unwrap();
        // A very important, very loud effect still cannot take either voice.
        assert!(m
            .play(&mut hw, &shot(Class::High, -1000), at(0x3FFF))
            .is_none());
        // Nor can another protected sound: the lane is full of them.
        assert!(m
            .play(&mut hw, &shot(Class::Protected, -1000), at(0x3FFF))
            .is_none());
        assert!(m.is_alive(p1) && m.is_alive(p2));
    }

    #[test]
    fn a_protected_sound_steals_an_ordinary_one_and_leaves_other_protected_alone() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        let p = m.play(&mut hw, &shot(Class::Protected, 0), at(10)).unwrap();
        let n = m.play(&mut hw, &shot(Class::Normal, 0), at(5000)).unwrap();
        let p2 = m.play(&mut hw, &shot(Class::Protected, 0), at(10)).unwrap();
        assert!(m.is_alive(p) && m.is_alive(p2));
        assert!(!m.is_alive(n));
        assert_eq!(p2.voice(), n.voice());
    }

    #[test]
    fn equal_importance_may_steal_if_at_least_as_loud_but_never_if_quieter() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let first = m.play(&mut hw, &shot(Class::Normal, 5), at(1000)).unwrap();
        // Quieter at the same importance: dropped, the loud sound stays.
        assert!(m.play(&mut hw, &shot(Class::Normal, 5), at(999)).is_none());
        assert!(m.is_alive(first));
        // Equally loud: allowed to take the voice.
        let second = m.play(&mut hw, &shot(Class::Normal, 5), at(1000)).unwrap();
        assert!(!m.is_alive(first));
        assert!(m.is_alive(second));
    }

    #[test]
    fn a_more_important_sound_steals_even_when_it_is_quieter() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let loud_low = m.play(&mut hw, &shot(Class::Low, 0), at(0x3000)).unwrap();
        let quiet_high = m.play(&mut hw, &shot(Class::High, 0), at(10)).unwrap();
        assert!(!m.is_alive(loud_low));
        assert!(m.is_alive(quiet_high));
    }

    #[test]
    fn a_less_important_sound_never_steals() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let high = m.play(&mut hw, &shot(Class::High, 0), at(10)).unwrap();
        assert!(m
            .play(&mut hw, &shot(Class::Normal, 0), at(0x3000))
            .is_none());
        assert!(m.play(&mut hw, &shot(Class::High, 1), at(0x3000)).is_none());
        assert!(m.is_alive(high));
    }

    #[test]
    fn a_loop_is_only_taken_by_something_strictly_more_important() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let engine = m
            .play(&mut hw, &looped(Class::Normal, 3), at(1000))
            .unwrap();
        assert!(m
            .play(&mut hw, &shot(Class::Normal, 3), at(0x3000))
            .is_none());
        assert!(m.is_alive(engine));
        let crash = m.play(&mut hw, &shot(Class::Normal, 2), at(1000)).unwrap();
        assert!(!m.is_alive(engine));
        assert!(m.is_alive(crash));
    }

    #[test]
    fn the_victim_is_the_least_important_then_the_quietest_then_the_one_closest_to_ending() {
        let mut m = smart(0b1111);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        // Voice 0: Low, loud. 1: Low, quiet, long. 2: Low, quiet, short. 3: Normal.
        let low_loud = m
            .play(&mut hw, &shot(Class::Low, 0), at(3000).with_ticks(50))
            .unwrap();
        let low_quiet_long = m
            .play(&mut hw, &shot(Class::Low, 0), at(100).with_ticks(50))
            .unwrap();
        let low_quiet_short = m
            .play(&mut hw, &shot(Class::Low, 0), at(100).with_ticks(20))
            .unwrap();
        let normal = m
            .play(&mut hw, &shot(Class::Normal, 0), at(10).with_ticks(50))
            .unwrap();
        // Importance first: the Normal sound survives although it is quietest.
        // Then quietest: not the loud Low. Then least time left: the short one.
        let new = m
            .play(&mut hw, &shot(Class::Normal, 0), at(500).with_ticks(50))
            .unwrap();
        assert_eq!(new.voice(), low_quiet_short.voice());
        assert!(m.is_alive(low_loud) && m.is_alive(low_quiet_long) && m.is_alive(normal));
    }

    #[test]
    fn a_voice_that_is_finishing_is_taken_before_a_victim_is_chosen() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        let long = m
            .play(&mut hw, &shot(Class::Low, 0), at(100).with_ticks(50))
            .unwrap();
        let ending = m
            .play(&mut hw, &shot(Class::Normal, 0), at(5000).with_ticks(10))
            .unwrap();
        m.tick(&mut hw, 9); // one tick to go
        let new = m.play(&mut hw, &shot(Class::Normal, 0), at(5000)).unwrap();
        assert_eq!(new.voice(), ending.voice());
        assert!(m.is_alive(long));
    }

    #[test]
    fn a_stale_handle_changes_nothing() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let old = m.play(&mut hw, &shot(Class::Normal, 5), at(1000)).unwrap();
        let new = m.play(&mut hw, &shot(Class::Normal, 5), at(1000)).unwrap();
        assert_ne!(old, new);
        hw.clear();
        assert!(!m.update(&mut hw, old, 1, 1));
        assert!(!m.stop(&mut hw, old));
        assert!(!m.set_pitch(&mut hw, old, 2000));
        assert!(!m.end_in(old, 1));
        assert!(hw.events().is_empty());
        assert!(m.is_alive(new));
    }

    #[test]
    fn a_handle_goes_stale_when_its_sound_ends() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        let h = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_ticks(4))
            .unwrap();
        m.tick(&mut hw, 3);
        assert!(m.is_alive(h));
        m.tick(&mut hw, 4);
        assert!(!m.is_alive(h));
        assert!(!m.update(&mut hw, h, 5, 5));
    }

    #[test]
    fn the_same_key_replaces_a_sound_in_place() {
        let mut m = smart(0b111);
        let mut hw = Rec::new();
        let a = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_key(42))
            .unwrap();
        let b = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_key(42))
            .unwrap();
        assert_eq!(a.voice(), b.voice());
        assert!(!m.is_alive(a));
        assert!(m.is_alive(b));
        // A different key takes a second voice.
        let c = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_key(43))
            .unwrap();
        assert_ne!(c.voice(), b.voice());
    }

    #[test]
    fn deadlines_survive_the_tick_counter_wrapping() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let start = u32::MAX - 2;
        m.tick(&mut hw, start);
        let h = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_ticks(5))
            .unwrap();
        m.tick(&mut hw, u32::MAX);
        assert!(m.is_alive(h));
        m.tick(&mut hw, 1); // start + 4
        assert!(m.is_alive(h));
        m.tick(&mut hw, 2); // start + 5
        assert!(!m.is_alive(h));
    }

    #[test]
    fn an_ended_one_shot_is_silenced_keyed_off_and_then_freed_after_its_tail() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        m.tick(&mut hw, 10);
        let h = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_ticks(2))
            .unwrap();
        hw.clear();
        m.tick(&mut hw, 12);
        assert_eq!(
            hw.events(),
            &[
                Event::Volume {
                    voice: 0,
                    left: 0,
                    right: 0
                },
                Event::KeyOff(1),
            ]
        );
        assert!(!m.is_alive(h));
        // Ringing: free to take, but not counted as idle.
        assert_eq!(m.free_in_lane(0), 1);
        m.tick(&mut hw, 12 + RELEASE_TICKS);
        assert_eq!(m.free_in_lane(0), 1);
        assert_eq!(m.playing(), 0);
    }

    #[test]
    fn an_idle_voice_is_preferred_to_one_still_ringing() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        let a = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_ticks(1))
            .unwrap();
        m.tick(&mut hw, 1); // a has ended and is ringing on voice a
        let b = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
        assert_ne!(b.voice(), a.voice());
    }

    #[test]
    fn inaudible_sounds_take_no_voice_unless_protected() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        m.set_cull(50);
        assert!(m.play(&mut hw, &shot(Class::Normal, 0), at(50)).is_none());
        assert!(m
            .play(&mut hw, &shot(Class::Normal, 0), Play::new(10, 50))
            .is_none());
        assert!(hw.events().is_empty());
        assert!(m
            .play(&mut hw, &shot(Class::Normal, 0), Play::new(10, 51))
            .is_some());
        let mut m = smart(0b1);
        m.set_cull(50);
        assert!(m.play(&mut hw, &shot(Class::Protected, 0), at(0)).is_some());
    }

    #[test]
    fn round_robin_cuts_the_oldest_and_skips_protected_sounds() {
        let mut m = Mixer::new([Lane::new(0b111, Policy::RoundRobin)], 60);
        let mut hw = Rec::new();
        let mut order = [0u8; 4];
        for o in order.iter_mut() {
            *o = m
                .play(&mut hw, &shot(Class::Normal, 0), at(100))
                .unwrap()
                .voice();
        }
        assert_eq!(order, [1, 2, 0, 1]);
        // Voice 2 is protected: the rotation steps over it.
        let mut m = Mixer::new([Lane::new(0b111, Policy::RoundRobin)], 60);
        m.play(&mut hw, &shot(Class::Normal, 0), at(100)); // voice 1
        let p = m
            .play(&mut hw, &shot(Class::Protected, 0), at(100))
            .unwrap(); // voice 2
        for _ in 0..4 {
            let h = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
            assert_ne!(h.voice(), p.voice());
        }
        assert!(m.is_alive(p));
    }

    #[test]
    fn first_free_never_steals() {
        let mut m = Mixer::new([Lane::new(0b11, Policy::FirstFree)], 60);
        let mut hw = Rec::new();
        let a = m.play(&mut hw, &looped(Class::Low, 100), at(10)).unwrap();
        let b = m.play(&mut hw, &looped(Class::Low, 100), at(10)).unwrap();
        assert_eq!((a.voice(), b.voice()), (0, 1));
        assert!(m
            .play(&mut hw, &shot(Class::Protected, -5), at(0x3000))
            .is_none());
        assert!(m.is_alive(a) && m.is_alive(b));
    }

    #[test]
    fn lanes_keep_their_voices_apart() {
        let mut m = Mixer::new(
            [
                Lane::new(0b0011, Policy::Smart),
                Lane::new(0b1100, Policy::Smart),
            ],
            60,
        );
        let mut hw = Rec::new();
        let ui = Sound::one_shot(SoundId(1), sample(), 0);
        let world = Sound::one_shot(SoundId(2), sample(), 1);
        for _ in 0..4 {
            let h = m.play(&mut hw, &ui, at(100)).unwrap();
            assert!(h.voice() < 2);
        }
        for _ in 0..4 {
            let h = m.play(&mut hw, &world, at(100)).unwrap();
            assert!((2..4).contains(&h.voice()));
        }
        assert!(m
            .play(&mut hw, &Sound::one_shot(SoundId(3), sample(), 9), at(100))
            .is_none());
    }

    #[test]
    #[should_panic(expected = "lanes must not share a voice")]
    fn overlapping_lanes_are_rejected() {
        let _ = Mixer::new(
            [
                Lane::new(0b011, Policy::Smart),
                Lane::new(0b110, Policy::Smart),
            ],
            60,
        );
    }

    #[test]
    #[should_panic(expected = "a lane uses a voice above 23")]
    fn a_lane_beyond_voice_23_is_rejected() {
        let _ = Mixer::new([Lane::new(1 << 24, Policy::Smart)], 60);
    }

    #[test]
    fn update_writes_only_when_the_levels_change() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        let h = m.play(&mut hw, &looped(Class::Normal, 0), at(100)).unwrap();
        hw.clear();
        assert!(m.update(&mut hw, h, 100, 100));
        assert!(hw.events().is_empty());
        assert!(m.update(&mut hw, h, 200, 50));
        assert_eq!(
            hw.events(),
            &[Event::Volume {
                voice: 0,
                left: 200,
                right: 50
            }]
        );
    }

    #[test]
    fn play_configures_sets_the_level_then_keys_on_in_that_order() {
        let mut m = smart(0b100);
        let mut hw = Rec::new();
        m.play(&mut hw, &shot(Class::Normal, 0), Play::new(1234, -77));
        assert_eq!(
            hw.events(),
            &[
                Event::Configure {
                    voice: 2,
                    addr: 0x1010,
                    rate_hz: 22050,
                    looped: false,
                    pitch_q12: 4096
                },
                Event::Volume {
                    voice: 2,
                    left: 1234,
                    right: -77
                },
                Event::KeyOn(1 << 2),
            ]
        );
    }

    #[test]
    fn the_master_volume_scales_new_sounds_and_rescales_live_ones() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        let mut master = Master::new();
        master.set_sfx(Ratio::new(1, 2));
        m.set_master(&mut hw, master);
        let h = m
            .play(&mut hw, &looped(Class::Normal, 0), Play::new(1000, -3001))
            .unwrap();
        assert_eq!(hw.last_volume(h.voice()), Some((500, -1500)));
        let mut louder = Master::new();
        louder.set_sfx(Ratio::new(2, 1));
        m.set_master(&mut hw, louder);
        assert_eq!(hw.last_volume(h.voice()), Some((2000, -6002)));
        // Unscaled levels are what update compares, so the slider is reversible.
        m.set_master(&mut hw, Master::new());
        assert_eq!(hw.last_volume(h.voice()), Some((1000, -3001)));
    }

    #[test]
    fn a_ducking_sound_pulls_the_music_down_and_lets_it_back() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        let mut master = Master::new();
        master.set_duck_rates(1024, 512);
        m.set_master(&mut hw, master);
        m.tick(&mut hw, 0);
        let horn = shot(Class::Protected, 0).with_duck(1024);
        m.play(&mut hw, &horn, at(100).with_ticks(6)).unwrap();
        m.tick(&mut hw, 1);
        assert_eq!(m.master().duck_gain_q12(), 3072);
        for t in 2..6 {
            m.tick(&mut hw, t);
        }
        assert_eq!(m.master().duck_gain_q12(), 1024);
        // The horn ends on tick 6: the music climbs back at the release rate.
        m.tick(&mut hw, 6);
        assert_eq!(m.master().duck_gain_q12(), 1536);
        for t in 7..12 {
            m.tick(&mut hw, t);
        }
        assert_eq!(m.master().duck_gain_q12(), 4096);
    }

    #[test]
    fn pitch_stretches_the_cutoff_and_is_held_to_the_hardware_ceiling() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        let plain = m.play(&mut hw, &shot(Class::Normal, 0), at(100)).unwrap();
        let half = m
            .play(&mut hw, &shot(Class::Normal, 0), at(100).with_pitch(2048))
            .unwrap();
        // 32 ticks plain, twice that at half pitch.
        m.tick(&mut hw, 31);
        assert!(m.is_alive(plain) && m.is_alive(half));
        m.tick(&mut hw, 32);
        assert!(!m.is_alive(plain) && m.is_alive(half));
        m.tick(&mut hw, 63);
        assert!(m.is_alive(half));
        m.tick(&mut hw, 64);
        assert!(!m.is_alive(half));

        // 22050 Hz may go up to 0x3FFF * 44100 / 22050 = 32766 in Q12.
        hw.clear();
        let mut m = smart(0b1);
        m.play(&mut hw, &shot(Class::Normal, 0), at(100).with_pitch(60000));
        assert!(matches!(
            hw.events()[0],
            Event::Configure {
                pitch_q12: 32766,
                ..
            }
        ));
    }

    #[test]
    fn stopping_a_range_silences_only_samples_inside_it() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        let inside = Sound::looped(
            SoundId(1),
            Sample::resident(SpuAddr::new(0x4000), 22050, 8),
            0,
        );
        let outside = Sound::looped(
            SoundId(2),
            Sample::resident(SpuAddr::new(0x9000), 22050, 8),
            0,
        );
        let a = m.play(&mut hw, &inside, at(100)).unwrap();
        let b = m.play(&mut hw, &outside, at(100)).unwrap();
        m.stop_range(&mut hw, 0x4000, 0x8000);
        assert!(!m.is_alive(a));
        assert!(m.is_alive(b));
        m.stop_all(&mut hw);
        assert_eq!(m.playing(), 0);
    }

    #[test]
    fn a_looped_sound_has_no_deadline_and_a_sample_of_unknown_length_none_either() {
        let mut m = smart(0b11);
        let mut hw = Rec::new();
        m.tick(&mut hw, 0);
        let l = m.play(&mut hw, &looped(Class::Normal, 0), at(100)).unwrap();
        let unknown = Sound::one_shot(
            SoundId(5),
            Sample::resident(SpuAddr::new(0x1010), 22050, 0),
            0,
        );
        let u = m.play(&mut hw, &unknown, at(100)).unwrap();
        m.tick(&mut hw, 100_000);
        assert!(m.is_alive(l) && m.is_alive(u));
    }

    #[test]
    fn ticking_an_idle_mixer_does_nothing() {
        let mut m = smart(0b1);
        let mut hw = Rec::new();
        for t in 0..1000 {
            m.tick(&mut hw, t);
        }
        assert!(hw.events().is_empty());
    }
}
