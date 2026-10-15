//! The seam between sound bookkeeping and the SPU.
//!
//! Everything above layer 0 in this crate ([`crate::mixer`], [`crate::vloop`])
//! decides *what* the 24 voices should be doing and never touches a register
//! itself. It reports each decision through [`Hw`]. On the console an
//! implementation of [`Hw`] forwards to `psx_spu::Voice`; on a host, and in
//! emulator golden tests, [`Recorder`] logs the calls in order so a test can
//! assert on exactly what would have been written, and in which order.
//!
//! The trait is deliberately the same size as the SPU's own per-voice surface,
//! so each method is one to three register writes and a recording is a faithful
//! write trace.

use crate::Sample;

/// What the mixer needs from a sound chip.
///
/// Voice numbers are `0..24`. Masks have bit `n` set for voice `n`.
///
/// # Example
///
/// ```
/// use psx_sfx::hw::{Hw, NullHw};
///
/// let mut hw = NullHw;
/// hw.key_off(0b101); // accepted and ignored
/// ```
pub trait Hw {
    /// Point `voice` at `sample` without starting it. `looped` selects the
    /// hardware-loop setup; a one-shot also gets its repeat address written so
    /// it parks on silence. `pitch_q12` is a multiplier of the cooked rate
    /// (`4096` plays it as recorded).
    fn configure(&mut self, voice: u8, sample: &Sample, looped: bool, pitch_q12: u16);

    /// Write a voice's left and right volume (signed SPU levels, already
    /// scaled by the master volume).
    fn set_volume(&mut self, voice: u8, left: i16, right: i16);

    /// Change a playing voice's pitch multiplier (`4096` = as cooked).
    fn set_pitch(&mut self, voice: u8, pitch_q12: u16);

    /// Key on every voice in `mask`.
    fn key_on(&mut self, mask: u32);

    /// Key off (release) every voice in `mask`.
    fn key_off(&mut self, mask: u32);
}

/// An [`Hw`] that ignores everything. Useful when only the bookkeeping is
/// being measured.
///
/// # Example
///
/// ```
/// use psx_sfx::hw::{Hw, NullHw};
///
/// NullHw.key_on(1);
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct NullHw;

impl Hw for NullHw {
    fn configure(&mut self, _voice: u8, _sample: &Sample, _looped: bool, _pitch_q12: u16) {}
    fn set_volume(&mut self, _voice: u8, _left: i16, _right: i16) {}
    fn set_pitch(&mut self, _voice: u8, _pitch_q12: u16) {}
    fn key_on(&mut self, _mask: u32) {}
    fn key_off(&mut self, _mask: u32) {}
}

/// One call made through [`Hw`], as [`Recorder`] logged it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// [`Hw::configure`].
    Configure {
        /// Voice number.
        voice: u8,
        /// Byte offset of the sample in SPU RAM.
        addr: u32,
        /// The rate the sample was cooked at.
        rate_hz: u32,
        /// Hardware-loop setup rather than one-shot setup.
        looped: bool,
        /// Pitch multiplier, `4096` = as cooked.
        pitch_q12: u16,
    },
    /// [`Hw::set_volume`].
    Volume {
        /// Voice number.
        voice: u8,
        /// Left level.
        left: i16,
        /// Right level.
        right: i16,
    },
    /// [`Hw::set_pitch`].
    Pitch {
        /// Voice number.
        voice: u8,
        /// Pitch multiplier, `4096` = as cooked.
        pitch_q12: u16,
    },
    /// [`Hw::key_on`] with this voice mask.
    KeyOn(u32),
    /// [`Hw::key_off`] with this voice mask.
    KeyOff(u32),
}

/// An [`Hw`] that logs every call into a fixed buffer, in order.
///
/// It needs no heap, so it also runs on the console, where a golden test can
/// compare the trace of two builds. When more than `CAP` calls arrive the
/// extra ones are counted in [`dropped`](Self::dropped) and not stored.
///
/// # Example
///
/// ```
/// use psx_sfx::hw::{Event, Hw, Recorder};
///
/// let mut rec = Recorder::<8>::new();
/// rec.set_volume(3, 100, -100);
/// rec.key_on(1 << 3);
/// assert_eq!(
///     rec.events(),
///     &[
///         Event::Volume { voice: 3, left: 100, right: -100 },
///         Event::KeyOn(1 << 3),
///     ]
/// );
/// ```
#[derive(Clone, Debug)]
pub struct Recorder<const CAP: usize> {
    events: [Event; CAP],
    len: usize,
    dropped: u32,
}

impl<const CAP: usize> Recorder<CAP> {
    /// An empty recorder.
    ///
    /// ```
    /// # use psx_sfx::hw::Recorder;
    /// assert!(Recorder::<4>::new().events().is_empty());
    /// ```
    pub const fn new() -> Self {
        Self {
            events: [Event::KeyOn(0); CAP],
            len: 0,
            dropped: 0,
        }
    }

    /// Everything logged so far, oldest first.
    ///
    /// ```
    /// # use psx_sfx::hw::{Event, Hw, Recorder};
    /// let mut r = Recorder::<4>::new();
    /// r.key_off(2);
    /// assert_eq!(r.events(), &[Event::KeyOff(2)]);
    /// ```
    pub fn events(&self) -> &[Event] {
        &self.events[..self.len]
    }

    /// Calls that arrived after the buffer filled and were not stored.
    ///
    /// ```
    /// # use psx_sfx::hw::{Hw, Recorder};
    /// let mut r = Recorder::<1>::new();
    /// r.key_on(1);
    /// r.key_on(2);
    /// assert_eq!(r.dropped(), 1);
    /// ```
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Forget everything logged so far.
    ///
    /// ```
    /// # use psx_sfx::hw::{Hw, Recorder};
    /// let mut r = Recorder::<4>::new();
    /// r.key_on(1);
    /// r.clear();
    /// assert!(r.events().is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }

    /// Union of every voice that was keyed on since the last
    /// [`clear`](Self::clear).
    ///
    /// ```
    /// # use psx_sfx::hw::{Hw, Recorder};
    /// let mut r = Recorder::<4>::new();
    /// r.key_on(0b01);
    /// r.key_on(0b10);
    /// assert_eq!(r.keyed_on(), 0b11);
    /// ```
    pub fn keyed_on(&self) -> u32 {
        self.events()
            .iter()
            .fold(0, |m, e| if let Event::KeyOn(k) = e { m | k } else { m })
    }

    /// Union of every voice that was keyed off since the last
    /// [`clear`](Self::clear).
    ///
    /// ```
    /// # use psx_sfx::hw::{Hw, Recorder};
    /// let mut r = Recorder::<4>::new();
    /// r.key_off(0b100);
    /// assert_eq!(r.keyed_off(), 0b100);
    /// ```
    pub fn keyed_off(&self) -> u32 {
        self.events()
            .iter()
            .fold(0, |m, e| if let Event::KeyOff(k) = e { m | k } else { m })
    }

    /// The most recent volume written to `voice`, if any.
    ///
    /// ```
    /// # use psx_sfx::hw::{Hw, Recorder};
    /// let mut r = Recorder::<4>::new();
    /// r.set_volume(5, 10, 20);
    /// assert_eq!(r.last_volume(5), Some((10, 20)));
    /// assert_eq!(r.last_volume(6), None);
    /// ```
    pub fn last_volume(&self, voice: u8) -> Option<(i16, i16)> {
        self.events().iter().rev().find_map(|e| match *e {
            Event::Volume {
                voice: v,
                left,
                right,
            } if v == voice => Some((left, right)),
            _ => None,
        })
    }

    fn push(&mut self, e: Event) {
        if self.len < CAP {
            self.events[self.len] = e;
            self.len += 1;
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

impl<const CAP: usize> Default for Recorder<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> Hw for Recorder<CAP> {
    fn configure(&mut self, voice: u8, sample: &Sample, looped: bool, pitch_q12: u16) {
        self.push(Event::Configure {
            voice,
            addr: sample.addr().byte_offset(),
            rate_hz: sample.sample_rate_hz(),
            looped,
            pitch_q12,
        });
    }

    fn set_volume(&mut self, voice: u8, left: i16, right: i16) {
        self.push(Event::Volume { voice, left, right });
    }

    fn set_pitch(&mut self, voice: u8, pitch_q12: u16) {
        self.push(Event::Pitch { voice, pitch_q12 });
    }

    fn key_on(&mut self, mask: u32) {
        self.push(Event::KeyOn(mask));
    }

    fn key_off(&mut self, mask: u32) {
        self.push(Event::KeyOff(mask));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_recorder_counts_what_it_could_not_keep() {
        let mut r = Recorder::<2>::new();
        r.key_on(1);
        r.key_on(2);
        r.key_off(4);
        r.key_off(8);
        assert_eq!(r.events(), &[Event::KeyOn(1), Event::KeyOn(2)]);
        assert_eq!(r.dropped(), 2);
        r.clear();
        assert!(r.events().is_empty());
        assert_eq!(r.dropped(), 0);
    }

    #[test]
    fn the_recorder_summarises_key_ons_and_the_latest_volume() {
        let mut r = Recorder::<16>::new();
        r.key_on(0b001);
        r.key_on(0b100);
        r.key_off(0b010);
        r.set_volume(2, 1, 2);
        r.set_volume(2, 3, 4);
        r.set_pitch(2, 5000);
        assert_eq!(r.keyed_on(), 0b101);
        assert_eq!(r.keyed_off(), 0b010);
        assert_eq!(r.last_volume(2), Some((3, 4)));
        assert_eq!(r.last_volume(3), None);
    }
}
