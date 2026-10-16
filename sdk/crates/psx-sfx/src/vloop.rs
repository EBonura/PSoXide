//! Virtual loops: many loops, few voices.
//!
//! Half-Life authors thirty-two ambient loops on seven voices, Quake thirty-two
//! on eleven, WipEout a crowd plus rival engines of which only the nearest two
//! sound. The shape is always the same: a table of loop emitters, each with the
//! level it would have if it had a voice, and a rule that gives the voices to the
//! loudest. [`LoopTable`] is that rule.
//!
//! A loop only changes hands when a waiting loop beats the weakest holder by a
//! margin (25 percent), so two loops of nearly equal level do not trade voices
//! every frame and click.
//!
//! The lane the loops play on should use [`Policy::FirstFree`](crate::mixer::Policy)
//! and hold nothing else: the table does its own stealing, with the margin, and
//! must not have the mixer do it behind its back.

use crate::hw::Hw;
use crate::mixer::{Handle, Mixer, Play, Sound};

/// How much louder (in percent) a waiting loop must be than the weakest loop
/// holding a voice before it takes that voice. Half-Life's `claim_if_louder`.
pub const HYSTERESIS_PERCENT: i32 = 25;

#[derive(Clone, Copy, Debug)]
struct Entry {
    sound: Option<Sound>,
    left: i16,
    right: i16,
    handle: Option<Handle>,
}

impl Entry {
    const EMPTY: Entry = Entry {
        sound: None,
        left: 0,
        right: 0,
        handle: None,
    };

    fn audibility(&self) -> i32 {
        (self.left.max(self.right) as i32).max(0)
    }
}

/// A table of `M` loop emitters sharing the voices of one lane.
///
/// # Example
///
/// ```
/// use psx_sfx::hw::Recorder;
/// use psx_sfx::mixer::*;
/// use psx_sfx::vloop::LoopTable;
/// use psx_sfx::Sample;
/// use psx_spu::SpuAddr;
///
/// // Two voices for ambient loops, three loops competing for them.
/// let mut mixer = Mixer::new([Lane::new(0b11, Policy::FirstFree)], 60);
/// let mut hw = Recorder::<64>::new();
/// let wind = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
///
/// let mut loops = LoopTable::<3>::new();
/// loops.set(0, &wind, 4000, 4000);
/// loops.set(1, &wind, 2000, 2000);
/// loops.set(2, &wind, 500, 500);
/// loops.update(&mut mixer, &mut hw, 4);
/// assert!(loops.handle(0).is_some() && loops.handle(1).is_some());
/// assert!(loops.handle(2).is_none()); // the quietest waits
/// ```
#[derive(Clone, Debug)]
pub struct LoopTable<const M: usize> {
    entries: [Entry; M],
}

impl<const M: usize> LoopTable<M> {
    /// An empty table.
    ///
    /// ```
    /// # use psx_sfx::vloop::LoopTable;
    /// let t = LoopTable::<8>::new();
    /// assert!(t.handle(0).is_none());
    /// ```
    pub const fn new() -> Self {
        Self {
            entries: [Entry::EMPTY; M],
        }
    }

    /// Register loop `index`, or refresh the levels it would have. The levels
    /// are what the loop would sound like with a voice; a loop at or below the
    /// mixer's cull threshold gives its voice up at the next
    /// [`update`](Self::update).
    pub fn set(&mut self, index: usize, sound: &Sound, left: i16, right: i16) {
        let e = &mut self.entries[index];
        e.sound = Some(*sound);
        e.left = left;
        e.right = right;
    }

    /// The handle of the voice loop `index` currently holds, if any.
    ///
    /// ```
    /// # use psx_sfx::vloop::LoopTable;
    /// assert!(LoopTable::<2>::new().handle(1).is_none());
    /// ```
    pub fn handle(&self, index: usize) -> Option<Handle> {
        self.entries[index].handle
    }

    /// Remove loop `index`, stopping its voice if it has one.
    ///
    /// ```
    /// # use psx_sfx::{hw::NullHw, mixer::*, vloop::LoopTable, Sample};
    /// # use psx_spu::SpuAddr;
    /// # let mut mixer = Mixer::new([Lane::new(1, Policy::FirstFree)], 60);
    /// # let sound = Sound::looped(SoundId(1), Sample::resident(SpuAddr::new(0x2000), 11025, 8), 0);
    /// let mut loops = LoopTable::<1>::new();
    /// loops.set(0, &sound, 900, 900);
    /// loops.update(&mut mixer, &mut NullHw, 1);
    /// loops.clear(&mut mixer, &mut NullHw, 0);
    /// assert_eq!(mixer.playing(), 0);
    /// ```
    pub fn clear<H: Hw, const L: usize>(&mut self, mixer: &mut Mixer<L>, hw: &mut H, index: usize) {
        let e = &mut self.entries[index];
        if let Some(h) = e.handle.take() {
            mixer.stop(hw, h);
        }
        *e = Entry::EMPTY;
    }

    /// Remove every loop, stopping the voices they hold.
    ///
    /// ```
    /// # use psx_sfx::{hw::NullHw, mixer::*, vloop::LoopTable};
    /// # let mut mixer = Mixer::new([Lane::new(1, Policy::FirstFree)], 60);
    /// let mut loops = LoopTable::<4>::new();
    /// loops.clear_all(&mut mixer, &mut NullHw);
    /// ```
    pub fn clear_all<H: Hw, const L: usize>(&mut self, mixer: &mut Mixer<L>, hw: &mut H) {
        for i in 0..M {
            self.clear(mixer, hw, i);
        }
    }

    /// Refresh the levels of loops that hold a voice, release the voices of
    /// loops that fell silent, and hand free or weakly held voices to waiting
    /// loops, at most `max_changes` hand-overs per call.
    ///
    /// Cost is linear in `M` plus a scan per hand-over; call it every second
    /// to fourth frame rather than every frame.
    pub fn update<H: Hw, const L: usize>(
        &mut self,
        mixer: &mut Mixer<L>,
        hw: &mut H,
        max_changes: usize,
    ) {
        let cull = mixer.cull() as i32;
        for e in self.entries.iter_mut() {
            let Some(h) = e.handle else { continue };
            if !mixer.is_alive(h) {
                e.handle = None; // stolen, or stopped elsewhere
            } else if e.audibility() <= cull {
                mixer.stop(hw, h);
                e.handle = None;
            } else {
                mixer.update(hw, h, e.left, e.right);
            }
        }

        let mut changes = 0;
        while changes < max_changes {
            let Some(waiting) = self.loudest_waiting(cull) else {
                break;
            };
            let Some(sound) = self.entries[waiting].sound else {
                break;
            };
            let lane = lane_of(&sound);
            if !mixer.has_free(lane) {
                let Some(weak) = self.weakest_holder(lane) else {
                    break;
                };
                let need = self.entries[weak].audibility() * (100 + HYSTERESIS_PERCENT) / 100;
                if self.entries[waiting].audibility() <= need {
                    break;
                }
                if let Some(h) = self.entries[weak].handle.take() {
                    mixer.stop(hw, h);
                }
            }
            let e = &mut self.entries[waiting];
            e.handle = mixer.play(hw, &sound, Play::new(e.left, e.right));
            if e.handle.is_none() {
                break; // the lane refused it; do not spin on it
            }
            changes += 1;
        }
    }

    fn loudest_waiting(&self, cull: i32) -> Option<usize> {
        let mut best: Option<(usize, i32)> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if e.sound.is_some() && e.handle.is_none() {
                let a = e.audibility();
                if a > cull && best.is_none_or(|(_, b)| a > b) {
                    best = Some((i, a));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn weakest_holder(&self, lane: usize) -> Option<usize> {
        let mut weak: Option<(usize, i32)> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if e.handle.is_some() && e.sound.as_ref().map(lane_of) == Some(lane) {
                let a = e.audibility();
                if weak.is_none_or(|(_, w)| a < w) {
                    weak = Some((i, a));
                }
            }
        }
        weak.map(|(i, _)| i)
    }
}

impl<const M: usize> Default for LoopTable<M> {
    fn default() -> Self {
        Self::new()
    }
}

fn lane_of(sound: &Sound) -> usize {
    sound.lane() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::Recorder;
    use crate::mixer::{Lane, Policy, SoundId};
    use crate::Sample;
    use psx_spu::SpuAddr;

    type Rec = Recorder<512>;

    fn wind() -> Sound {
        Sound::looped(
            SoundId(1),
            Sample::resident(SpuAddr::new(0x2000), 11025, 8),
            0,
        )
    }

    fn rig(voices: u32) -> (Mixer<1>, Rec) {
        (
            Mixer::new([Lane::new(voices, Policy::FirstFree)], 60),
            Rec::new(),
        )
    }

    #[test]
    fn the_loudest_loops_get_the_voices() {
        let (mut m, mut hw) = rig(0b11);
        let mut t = LoopTable::<4>::new();
        for (i, a) in [100, 4000, 300, 2500].into_iter().enumerate() {
            t.set(i, &wind(), a, a);
        }
        t.update(&mut m, &mut hw, 8);
        let held: [bool; 4] = core::array::from_fn(|i| t.handle(i).is_some());
        assert_eq!(held, [false, true, false, true]);
    }

    #[test]
    fn a_waiting_loop_must_beat_the_weakest_holder_by_the_margin() {
        let (mut m, mut hw) = rig(0b1);
        let mut t = LoopTable::<2>::new();
        t.set(0, &wind(), 1000, 1000);
        t.set(1, &wind(), 1100, 1100); // louder, but not by 25 percent
        t.update(&mut m, &mut hw, 8);
        // Loop 1 is louder so it was served first; loop 0 waits.
        assert!(t.handle(1).is_some() && t.handle(0).is_none());
        // Loop 0 draws level with the holder: no swap, however often we update.
        for _ in 0..10 {
            t.set(0, &wind(), 1200, 1200);
            t.update(&mut m, &mut hw, 8);
            assert!(t.handle(1).is_some() && t.handle(0).is_none());
        }
        // Clearly louder (more than 1100 * 1.25 = 1375): it takes the voice.
        t.set(0, &wind(), 1400, 1400);
        t.update(&mut m, &mut hw, 8);
        assert!(t.handle(0).is_some() && t.handle(1).is_none());
    }

    #[test]
    fn two_nearly_equal_loops_do_not_trade_voices_frame_after_frame() {
        let (mut m, mut hw) = rig(0b1);
        let mut t = LoopTable::<2>::new();
        t.set(0, &wind(), 1000, 1000);
        t.set(1, &wind(), 1010, 1010);
        t.update(&mut m, &mut hw, 8);
        hw.clear();
        for frame in 0..200 {
            let wobble = (frame % 7) as i16 * 10;
            t.set(0, &wind(), 1000 + wobble, 1000 + wobble);
            t.set(1, &wind(), 1040 - wobble, 1040 - wobble);
            t.update(&mut m, &mut hw, 8);
        }
        assert_eq!(hw.keyed_on(), 0, "no key-on after the first assignment");
    }

    #[test]
    fn a_loop_that_falls_silent_gives_its_voice_to_a_waiting_one() {
        let (mut m, mut hw) = rig(0b1);
        let mut t = LoopTable::<2>::new();
        t.set(0, &wind(), 3000, 3000);
        t.set(1, &wind(), 500, 500);
        t.update(&mut m, &mut hw, 8);
        assert!(t.handle(0).is_some() && t.handle(1).is_none());
        t.set(0, &wind(), 0, 0);
        t.update(&mut m, &mut hw, 8);
        assert!(t.handle(0).is_none() && t.handle(1).is_some());
    }

    #[test]
    fn held_loops_follow_their_levels() {
        let (mut m, mut hw) = rig(0b1);
        let mut t = LoopTable::<1>::new();
        t.set(0, &wind(), 1000, 2000);
        t.update(&mut m, &mut hw, 1);
        t.set(0, &wind(), 1500, 500);
        t.update(&mut m, &mut hw, 1);
        assert_eq!(hw.last_volume(0), Some((1500, 500)));
    }

    #[test]
    fn a_voice_taken_elsewhere_is_noticed_and_the_loop_waits_again() {
        let (mut m, mut hw) = rig(0b1);
        let mut t = LoopTable::<1>::new();
        t.set(0, &wind(), 1000, 1000);
        t.update(&mut m, &mut hw, 1);
        let h = t.handle(0).unwrap();
        m.stop(&mut hw, h);
        // The mixer is still ringing; the table drops the dead handle and
        // re-claims the voice in the same update.
        t.update(&mut m, &mut hw, 1);
        let again = t.handle(0).unwrap();
        assert_ne!(again, h);
        assert!(m.is_alive(again));
    }

    #[test]
    fn at_most_max_changes_loops_are_started_per_update() {
        let (mut m, mut hw) = rig(0b1111);
        let mut t = LoopTable::<4>::new();
        for i in 0..4 {
            t.set(i, &wind(), 1000 + i as i16, 1000);
        }
        t.update(&mut m, &mut hw, 2);
        assert_eq!(m.playing().count_ones(), 2);
        t.update(&mut m, &mut hw, 2);
        assert_eq!(m.playing().count_ones(), 4);
    }

    #[test]
    fn clearing_a_loop_stops_its_voice() {
        let (mut m, mut hw) = rig(0b11);
        let mut t = LoopTable::<2>::new();
        t.set(0, &wind(), 1000, 1000);
        t.set(1, &wind(), 1000, 1000);
        t.update(&mut m, &mut hw, 4);
        assert_eq!(m.playing().count_ones(), 2);
        t.clear(&mut m, &mut hw, 0);
        assert_eq!(m.playing().count_ones(), 1);
        t.clear_all(&mut m, &mut hw);
        assert_eq!(m.playing(), 0);
        assert!(t.handle(0).is_none() && t.handle(1).is_none());
    }
}
