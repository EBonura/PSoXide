//! WipEout's retail note owner, pinned by the tests that came with it.
//!
//! WipEout's PAL sound driver (`PlayNote` at 8003f2fc, `UpdateNotes` at
//! 8003ec80) shares channels 1 to 23 between effects and loops, never uses
//! channel 0, and decides everything audible from three numbers per channel
//! and a program-to-channel table. [`Owner`] is that logic with no hardware in
//! it. It is the reference a [`Policy::StrictRetail`](crate::mixer::Policy)
//! lane is held to: the differential test in this module drives an `Owner` and
//! a `StrictRetail` mixer lane through the same long sequence and requires the
//! same channel every time.
//!
//! What an `Owner` has that a mixer lane does not is retail's bookkeeping around
//! the allocation: the program-to-channel table that [`Owner::kill`] follows
//! even after the channel was reused, the eight-update release, the fade
//! window, and the pause save and restore. A game replaying retail behaviour
//! uses `Owner` for those and the lane for everything else.
//!
//! The shared policy and the duration cadence are confirmed against the PAL
//! retail build; the NTSC retail build has not been checked.
//!
//! # Example
//!
//! ```
//! use psx_sfx::retail::Owner;
//!
//! let mut owner = Owner::new();
//! // A loop (duration -1) takes channel 1 and is never stolen.
//! assert_eq!(owner.play(19, 0, -1), Some(1));
//! // An effect of priority 1 lasting 20 updates takes the next free channel.
//! assert_eq!(owner.play(7, 1, 20), Some(2));
//! ```

/// Program identities: 35 original, plus 7 for the second local player.
pub const PROGRAMS: usize = 42;

/// Updates a killed note keeps playing before it is released.
pub const NOTE_RELEASE: i32 = 8;

/// Updates before the end of a note during which its volume fades.
pub const NOTE_FADE_STEPS: i32 = 9;

/// One channel of the owner.
#[derive(Copy, Clone)]
pub struct Channel {
    /// Updates left: `0` is free, `-1` is a loop, positive is a finite note.
    pub duration: i32,
    /// Priority; a smaller number is more important.
    pub priority: i16,
}

const EMPTY: Channel = Channel {
    duration: 0,
    priority: 0,
};

/// The channel table and program map.
pub struct Owner {
    /// The 24 channels; channel 0 is never allocated.
    pub channels: [Channel; 24],
    /// The last channel each program was given, kept after replacement.
    pub latest: [u8; PROGRAMS],
    paused: bool,
}

/// What one [`Owner::update`] asks the hardware to do.
#[derive(Default)]
pub struct Update {
    /// Channels to key off.
    pub key_off: u32,
    /// Channels whose volume fades one step.
    pub fade: u32,
    /// Fade shift: 2 playing, 3 paused.
    pub shift: u8,
    /// Save every channel's volume (pause began).
    pub save: bool,
    /// Restore the saved volumes (pause ended).
    pub restore: bool,
}

impl Owner {
    /// An owner with every channel free.
    pub const fn new() -> Self {
        Self {
            channels: [EMPTY; 24],
            latest: [0; PROGRAMS],
            paused: false,
        }
    }

    /// Start a note for `program`: the first free channel, else the first
    /// finite note with a strictly lower priority (a larger number). Loops are
    /// never taken and equal priority cannot steal.
    ///
    /// ```
    /// # use psx_sfx::retail::Owner;
    /// let mut o = Owner::new();
    /// for _ in 1..24 {
    ///     o.play(7, 1, 20);
    /// }
    /// assert_eq!(o.play(7, 1, 20), None); // equal priority cannot steal
    /// assert_eq!(o.play(7, 0, 20), Some(1)); // a more important note can
    /// ```
    pub fn play(&mut self, program: u8, priority: i16, duration: i16) -> Option<u8> {
        let at = (1..24)
            .find(|&i| self.channels[i].duration == 0)
            .or_else(|| {
                (1..24).find(|&i| {
                    self.channels[i].duration > 0 && priority < self.channels[i].priority
                })
            })?;
        self.channels[at] = Channel {
            duration: duration as i32,
            priority,
        };
        self.latest[program as usize] = at as u8;
        Some(at as u8)
    }

    /// Native KillNote follows the program's last channel, even when its
    /// prior note ended or was replaced. It releases after eight updates.
    ///
    /// ```
    /// # use psx_sfx::retail::{Owner, NOTE_RELEASE};
    /// let mut o = Owner::new();
    /// let ch = o.play(3, 1, 50).unwrap();
    /// o.kill(3);
    /// assert_eq!(o.channels[ch as usize].duration, NOTE_RELEASE);
    /// ```
    pub fn kill(&mut self, program: u8) {
        self.channels[self.latest[program as usize] as usize].duration = NOTE_RELEASE;
    }

    /// Free every channel. Like the original KillAllFx it keeps the
    /// program-to-channel identities.
    ///
    /// ```
    /// # use psx_sfx::retail::Owner;
    /// let mut o = Owner::new();
    /// o.play(1, 0, -1);
    /// o.stop_all();
    /// assert_eq!(o.channels[1].duration, 0);
    /// ```
    pub fn stop_all(&mut self) {
        for channel in &mut self.channels {
            channel.duration = 0;
        }
        self.paused = false;
    }

    /// One `UpdateNotes` call. A change of `paused` returns a save or restore
    /// and no tick; otherwise every note ages by one, fading in its last
    /// [`NOTE_FADE_STEPS`] updates and keying off on its last.
    ///
    /// ```
    /// # use psx_sfx::retail::Owner;
    /// let mut o = Owner::new();
    /// o.play(1, 1, 1);
    /// assert_eq!(o.update(false).key_off, 1 << 1);
    /// ```
    pub fn update(&mut self, paused: bool) -> Update {
        if paused != self.paused {
            self.paused = paused;
            return Update {
                save: paused,
                restore: !paused,
                ..Update::default()
            };
        }
        let mut out = Update {
            shift: if paused { 3 } else { 2 },
            ..Update::default()
        };
        for i in 1..24 {
            let d = self.channels[i].duration;
            if paused || (1..=NOTE_FADE_STEPS).contains(&d) {
                out.fade |= 1 << i;
            }
            if d == 1 {
                out.key_off |= 1 << i;
            }
            if d > 0 {
                self.channels[i].duration -= 1;
            }
        }
        out
    }
}

impl Default for Owner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::Recorder;
    use crate::mixer::{Class, Lane, Mixer, Play, Policy, Sound, SoundId};
    use crate::Sample;
    use psx_spu::SpuAddr;

    // The three tests below are WipEout's own `audio_voices.rs` tests, moved
    // here unchanged apart from `crate::rate::NOTE_RELEASE`, which is
    // `NOTE_RELEASE` in this module.

    #[test]
    fn native_audio_shared_pool_keeps_loops_and_strict_signed_priority() {
        let mut owner = Owner::new();
        assert_eq!(owner.play(19, 0, -1), Some(1));
        assert_eq!(owner.play(30, 0, -1), Some(2));
        for i in 3..24 {
            assert_eq!(owner.play(7, 1, 20), Some(i));
        }
        assert_eq!(owner.play(0, 1, 30), None); // equal priority cannot steal
        assert_eq!(owner.play(0, -1, 30), Some(3));
        assert_eq!(
            (owner.channels[1].duration, owner.channels[2].duration),
            (-1, -1)
        );
        assert_eq!(owner.latest[0], 3);
    }

    #[test]
    fn native_audio_program_latest_identity_and_finite_release_are_independent_of_sample_end() {
        let mut owner = Owner::new();
        assert_eq!(owner.play(7, 1, 20), Some(1));
        assert_eq!(owner.play(7, 1, 20), Some(2));
        owner.kill(7);
        assert_eq!(
            (owner.channels[1].duration, owner.channels[2].duration),
            (20, 8)
        );
        for _ in 0..7 {
            assert_eq!(owner.update(false).key_off, 0);
        }
        let last = owner.update(false);
        assert_eq!(last.key_off, 1 << 2);
        assert_eq!(last.fade & (1 << 2), 1 << 2);
        assert_eq!(
            (owner.channels[1].duration, owner.channels[2].duration),
            (12, 0)
        );
        assert_eq!(owner.play(8, 1, 20), Some(2));
        // Native last-program identity is retained after replacement.
        owner.kill(7);
        assert_eq!(owner.channels[2].duration, 8);
    }

    #[test]
    fn native_audio_pause_transition_has_no_tick_and_preserves_reserved_loop() {
        let mut owner = Owner::new();
        owner.play(13, 0, -1);
        owner.play(26, 1, 2);
        let first = owner.update(true);
        assert!(first.save);
        assert_eq!(owner.channels[2].duration, 2);
        let paused = owner.update(true);
        assert_eq!(paused.shift, 3);
        assert_eq!(paused.fade, 0x00fffffe);
        assert_eq!(owner.channels[2].duration, 1);
        assert!(owner.update(false).restore);
        assert_eq!(owner.channels[2].duration, 1);
        let resumed = owner.update(false);
        assert_eq!(resumed.key_off, 1 << 2);
        assert_eq!(resumed.shift, 2);
        assert_eq!(owner.channels[1].duration, -1);
    }

    // ---- The same decisions through a StrictRetail mixer lane. ----

    const RETAIL: u32 = 0x00ff_fffe; // channels 1..=23

    fn sample() -> Sample {
        Sample::resident(SpuAddr::new(0x1010), 44100, 100)
    }

    fn retail_mixer() -> Mixer<1> {
        Mixer::new([Lane::new(RETAIL, Policy::StrictRetail)], 60)
    }

    fn sound(prio: i16, looped: bool) -> Sound {
        let s = if looped {
            Sound::looped(SoundId(0), sample(), 0)
        } else {
            Sound::one_shot(SoundId(0), sample(), 0)
        };
        s.with_class(Class::Normal).with_prio(prio)
    }

    #[test]
    fn a_strict_retail_lane_reproduces_the_shared_pool_test() {
        let mut m = retail_mixer();
        let mut hw = Recorder::<256>::new();
        let play = |m: &mut Mixer<1>, hw: &mut Recorder<256>, prio, looped, dur: u32| {
            m.play(
                hw,
                &sound(prio, looped),
                Play::new(100, 100).with_ticks(dur),
            )
            .map(|h| h.voice())
        };
        assert_eq!(play(&mut m, &mut hw, 0, true, 0), Some(1));
        assert_eq!(play(&mut m, &mut hw, 0, true, 0), Some(2));
        for i in 3..24 {
            assert_eq!(play(&mut m, &mut hw, 1, false, 20), Some(i));
        }
        assert_eq!(play(&mut m, &mut hw, 1, false, 30), None);
        assert_eq!(play(&mut m, &mut hw, -1, false, 30), Some(3));
        // The two loops are still alive: only channel 3 changed hands.
        assert_eq!(m.playing(), RETAIL);
    }

    #[test]
    fn a_strict_retail_note_ends_on_the_update_that_keys_it_off() {
        let mut m = retail_mixer();
        let mut hw = Recorder::<64>::new();
        m.tick(&mut hw, 0);
        let h = m
            .play(&mut hw, &sound(1, false), Play::new(100, 100).with_ticks(3))
            .unwrap();
        hw.clear();
        for now in 1..=2 {
            m.tick(&mut hw, now);
            assert_eq!(hw.keyed_off(), 0, "alive on update {now}");
        }
        m.tick(&mut hw, 3);
        assert_eq!(hw.keyed_off(), 1 << h.voice());
        // Retail frees the channel immediately and does not zero its volume.
        assert!(hw.last_volume(h.voice()).is_none());
        assert_eq!(m.playing(), 0);
    }

    /// A small deterministic generator, so the sequence is the same everywhere.
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0 >> 8
        }
    }

    #[test]
    fn a_strict_retail_lane_makes_the_same_choices_as_the_owner_over_a_long_sequence() {
        let (mut dropped, mut stolen, mut ended) = (0, 0, 0);
        for seed in 1..=8u32 {
            let mut rng = Lcg(seed);
            let mut owner = Owner::new();
            let mut m = retail_mixer();
            let mut hw = Recorder::<4>::new();
            let mut now = 0u32;
            m.tick(&mut hw, now);
            for step in 0..4000 {
                if rng.next().is_multiple_of(3) {
                    // One UpdateNotes: the owner ages its notes, the mixer's
                    // clock moves one tick.
                    let out = owner.update(false);
                    now += 1;
                    let before = m.playing();
                    m.tick(&mut hw, now);
                    let gone = before & !m.playing();
                    assert_eq!(gone, out.key_off, "seed {seed} step {step}: key-off set");
                    ended += gone.count_ones();
                } else {
                    let prio = (rng.next() % 5) as i16 - 2;
                    let looped = rng.next().is_multiple_of(9);
                    let dur = 1 + (rng.next() % 40) as i16;
                    let occupied = owner.channels.iter().skip(1).all(|c| c.duration != 0);
                    let want = owner.play(0, prio, if looped { -1 } else { dur });
                    match want {
                        None => dropped += 1,
                        Some(_) if occupied => stolen += 1,
                        Some(_) => {}
                    }
                    let got = m
                        .play(
                            &mut hw,
                            &sound(prio, looped),
                            Play::new(100, 100).with_ticks(dur as u32),
                        )
                        .map(|h| h.voice());
                    assert_eq!(got, want, "seed {seed} step {step}: channel");
                }
                // Channel occupancy agrees at every step.
                let mut live = 0u32;
                for (i, c) in owner.channels.iter().enumerate().skip(1) {
                    if c.duration != 0 {
                        live |= 1 << i;
                    }
                }
                assert_eq!(m.playing(), live, "seed {seed} step {step}: occupancy");
            }
        }
        // The sequence must actually exercise the interesting decisions.
        assert!(
            dropped > 100 && stolen > 100 && ended > 100,
            "{dropped} {stolen} {ended}"
        );
    }

    #[test]
    fn ending_a_note_early_follows_the_retail_release_length() {
        let mut m = retail_mixer();
        let mut hw = Recorder::<64>::new();
        m.tick(&mut hw, 100);
        let h = m
            .play(&mut hw, &sound(1, true), Play::new(100, 100))
            .unwrap();
        assert!(m.end_in(h, NOTE_RELEASE as u32));
        m.tick(&mut hw, 107);
        assert!(m.is_alive(h));
        m.tick(&mut hw, 108);
        assert!(!m.is_alive(h));
    }
}
