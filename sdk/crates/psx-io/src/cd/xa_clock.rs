// SPDX-License-Identifier: GPL-2.0-or-later
//! A song clock for XA-ADPCM music: where the playing song is, in
//! milliseconds, every frame, for beat sync, spectrum bars and rhythm games.
//!
//! The XA player has no sample counter to read. What the drive can say is
//! which sector its head is in ([`Player::poll`] asks with GetlocP), and a
//! song's sectors stream at a fixed rate (75 a second at single speed, 150
//! at double), so the head's place in the file *is* the song's time. The
//! clock turns that coarse, occasional reading into a smooth one the way
//! [`PlaybackClock`] does for CD-DA: it anchors on the reading and advances
//! with the caller's own tick counter in between, so the drive is asked
//! rarely (once per 15 to 30 ticks is plenty) and the display and CD
//! oscillators cannot drift apart for long.
//!
//! ```ignore
//! let mut clock = SongClock::new(60);
//! // once per tick:
//! player.poll();                               // or every 15..30 ticks
//! if let Some(ms) = clock.update(&player, tick) {
//!     // ms into the song file, smooth and never backwards
//! }
//! ```
//!
//! # Precision
//!
//! These are the errors this module can account for. All of them are reasoned
//! from how the clock works and checked against a simulated drive in the unit
//! tests; none is measured on a console.
//!
//! * **Sector quantum, 13.3 ms at single speed (6.7 ms at double).** The
//!   head reading is a whole sector. The clock takes the middle of it, so
//!   the error is at most half a sector either way (6.7 ms), and zero on
//!   average.
//! * **Tick granularity.** The anchor is stamped with the tick of the call,
//!   but the drive answers some milliseconds into the frame. Allow up to one
//!   tick (16.7 ms at 60 Hz) on top, one-sided: the clock reads slightly
//!   early. Call [`SongClock::update`] right after `poll`, at the same place
//!   in the frame every time, so the offset is constant.
//! * **Drift.** Between readings the clock runs on the tick counter. The
//!   display and CD crystals differ by well under 0.1 percent, so 30 ticks
//!   (half a second) drift a fraction of a millisecond, and every reading
//!   re-anchors it. Nothing accumulates over a song.
//! * **Never backwards.** Small corrections only move the clock forward and
//!   large ones are held until a second reading agrees (the
//!   [`PlaybackClock`] policy), so the output does not step back inside one
//!   run of the song. A loop or a resume starts a new run.
//! * **Head versus sound: not known.** The drive reads ahead of what the
//!   speakers play. The sector the head is in is decoded and queued at the
//!   SPU, which then needs time to play it out. The player's own doc says
//!   "a sector or two", and the XA decoder works in 4-sector groups, so a
//!   constant offset up to roughly 55 ms is plausible. Nothing in this crate
//!   can observe it and the emulator does not model it. It has to be
//!   measured on the console (the hardware-test suite's XA clock step is
//!   there for that) and put in with [`SongClock::with_head_lead_ms`]. Until
//!   then the clock reports where the *head* is, and the sound is behind it
//!   by that unmeasured amount.
//!
//! So: the clock is ahead of the sound by the unmeasured head lead, and
//! otherwise within about 7 ms either way plus up to 17 ms early. Rhythm
//! timing windows in the tens of milliseconds (gh-psx uses plus or minus
//! 90 ms) fit with a margin; sample-accurate sync does not.
//!
//! # What would sharpen it
//!
//! The SPU writes the CD input it is playing into its capture buffer in SPU
//! RAM, and that write position moves every 22.7 microseconds. Reading it
//! would pin the phase inside a sector. It is not used: reaching it needs an
//! SPU IRQ address or a RAM read-back that nothing in this SDK has proved on
//! silicon, and a 90 ms window does not need it. A per-sector interrupt (the
//! Report mode bit) would timestamp each sector boundary but needs a CD
//! interrupt handler, which the polled players avoid. Both are options if a
//! game ever needs under 10 ms.

use super::audio::PlaybackClock;
use super::xa::{HeadSample, Player};

/// Smooth, monotonic song time for the XA song a [`Player`] is playing.
pub struct SongClock {
    clock: PlaybackClock,
    epoch: Option<u32>,
    serial: u32,
    head_lead_ms: i32,
}

impl SongClock {
    /// A clock for a tick counter running at `ticks_hz` (60 NTSC, 50 PAL).
    pub const fn new(ticks_hz: u32) -> Self {
        Self {
            clock: PlaybackClock::new(ticks_hz),
            epoch: None,
            serial: 0,
            head_lead_ms: 0,
        }
    }

    /// Subtract `ms` from every reading: how far the drive head is ahead of
    /// the sound you hear. Measured on a console, not derived (see the module
    /// docs); negative moves the other way. Also the place for a
    /// player-facing audio latency setting.
    pub const fn with_head_lead_ms(mut self, ms: i32) -> Self {
        self.head_lead_ms = ms;
        self
    }

    /// Song time in milliseconds at `now_tick`, from the newest reading
    /// `player` holds. Call it every tick, after [`Player::poll`]. `None`
    /// while the song is not streaming (starting, seeking after a loop or a
    /// resume, suspended, stopped); the clock restarts from the first
    /// reading of each new run.
    pub fn update(&mut self, player: &Player, now_tick: u32) -> Option<u32> {
        self.update_with(player.head_sample(), now_tick)
    }

    fn update_with(&mut self, sample: Option<HeadSample>, now_tick: u32) -> Option<u32> {
        let sample = sample?;
        let reading = sample.centre_millis();
        let ms = if self.epoch != Some(sample.epoch) {
            self.epoch = Some(sample.epoch);
            self.serial = sample.serial;
            self.clock.start_at(now_tick, reading);
            reading
        } else if sample.serial != self.serial {
            self.serial = sample.serial;
            self.clock.observe(now_tick, Some(reading))
        } else {
            self.clock.observe(now_tick, None)
        };
        Some(if self.head_lead_ms >= 0 {
            ms.saturating_sub(self.head_lead_ms as u32)
        } else {
            ms.saturating_add(self.head_lead_ms.unsigned_abs())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: u32 = 75;

    /// The sample the drive gives at true song time `true_ms`: the sector the
    /// time falls in.
    fn sample(epoch: u32, serial: u32, true_ms: u32) -> HeadSample {
        HeadSample {
            epoch,
            serial,
            sector: true_ms * SPS / 1000,
            sectors_per_second: SPS,
        }
    }

    #[test]
    fn a_head_sample_is_centred_in_its_sector() {
        let s = HeadSample {
            epoch: 1,
            serial: 1,
            sector: 3,
            sectors_per_second: 75,
        };
        assert_eq!(s.millis(), 40);
        assert_eq!(s.centre_millis(), 46);
        let d = HeadSample {
            sectors_per_second: 150,
            ..s
        };
        assert_eq!(d.millis(), 20);
        assert_eq!(d.centre_millis(), 23);
    }

    #[test]
    fn nothing_is_reported_before_the_song_streams() {
        let mut clock = SongClock::new(60);
        assert_eq!(clock.update_with(None, 5), None);
    }

    /// The song starts mid-run on the tick counter; the drive is read every
    /// 30 ticks and answers in whole sectors. The output follows the true
    /// time to within half a sector plus a tick, never steps back, and does
    /// not drift over three minutes even though the tick counter runs 0.05
    /// percent slow against the song.
    #[test]
    fn follows_true_time_within_a_sector_and_a_tick_without_drift() {
        let mut clock = SongClock::new(60);
        let start_tick = 100;
        let mut serial = 0;
        let mut previous = 0;
        let mut worst = 0i32;
        for tick in start_tick..start_tick + 60 * 180 {
            // True song time at this tick; the song runs 0.05 percent ahead
            // of the tick counter.
            let true_ms = ((tick - start_tick) as u64 * 1000 / 60 * 10_005 / 10_000) as u32;
            if (tick - start_tick) % 30 == 3 {
                serial += 1;
            }
            let ms = clock
                .update_with(Some(sample(1, serial, true_ms)), tick)
                .expect("streaming");
            assert!(ms >= previous, "tick {tick}: {ms} after {previous}");
            previous = ms;
            worst = worst.max((ms as i32 - true_ms as i32).abs());
        }
        // Half a sector (7), one tick (17), the 30-tick drift of 0.05
        // percent (under 10) and rounding.
        assert!(worst <= 7 + 17 + 10 + 2, "worst error {worst} ms");
    }

    #[test]
    fn a_new_epoch_restarts_the_clock_and_forgets_the_old_floor() {
        let mut clock = SongClock::new(60);
        clock.update_with(Some(sample(1, 1, 50_000)), 10);
        let late = clock.update_with(Some(sample(1, 1, 50_000)), 70).unwrap();
        assert!(late >= 50_900, "{late}");
        // A loop: the new epoch's first reading is near the top.
        let top = clock.update_with(Some(sample(2, 5, 40)), 71).unwrap();
        assert!(top < 100, "restarted at {top}");
        let a_second_on = clock.update_with(Some(sample(2, 5, 40)), 131).unwrap();
        assert!((a_second_on - top).abs_diff(1000) <= 17, "{a_second_on}");
    }

    #[test]
    fn head_lead_is_subtracted_and_floors_at_zero() {
        let mut clock = SongClock::new(60).with_head_lead_ms(30);
        assert_eq!(clock.update_with(Some(sample(1, 1, 0)), 0), Some(0));
        let mut clock = SongClock::new(60).with_head_lead_ms(-20);
        assert_eq!(clock.update_with(Some(sample(1, 1, 0)), 0), Some(26));
    }
}
