// SPDX-License-Identifier: GPL-2.0-or-later
//! Pausing the drive and knowing it has stopped: the silicon protocol, once.
//!
//! Pause is a two-response command. The first response (INT3) comes
//! about 33,000 system clocks after the command and carries status `0x22`,
//! "still reading". The second (INT2) says the drive has actually stopped, and on
//! the project console it comes **a long time later**: out of a read, counted from
//! the first response, 5.33 sector periods at the first sector, 6.17 after a far
//! seek and 8.39 once the head has settled, i.e. 1.2 to 3.9 million system clocks
//! (hardware-tests v2.7, run FB89, records 0x340 to 0x3B8). An emulator that
//! completes a Pause in tens of thousands of clocks hides all of it, which is how
//! three games shipped code that gave up after a few hundred thousand and read
//! `0x22` as a failed stop (OoT froze in Link's House on the console, 2026-10-19).
//!
//! What else the console does, all measured:
//!
//! * A command sent between the two responses (GetStat, 4 of 4 trials) cancels the
//!   second one for good. Proving the stop by asking for the status therefore
//!   replaces waiting for INT2; it does not add to it.
//! * A Pause sent while a read is still seeking is refused with INT5 (status
//!   `0x03`, once `0x43`) and no INT3, six of six; the read carries on, so the
//!   Pause has to be sent again once the seek is over. The status byte cannot
//!   say when that is (it reads `0x03`, neither seeking nor reading, five times
//!   of six), but the first data sector of the read can.
//! * A GetStat at the first response reads `0x22` and one 230,000 clocks later
//!   `0x02`, long before INT2. The reading bit is therefore not a stop signal
//!   by itself, and [`pause_until_stopped`] uses it only after INT2 has failed
//!   to come.
//!
//! [`pause_until_stopped`] is the whole protocol over the small [`PauseDrive`]
//! trait, so the host tests can play the console's timings against it:
//! send, wait for INT2 up to [`COMPLETE_SPINS`], fall back to polling the status
//! when INT2 never shows, and on a refusal wait the seek out and send again.

use psx_hw::cd::{STAT_PLAYING, STAT_READING, STAT_SEEKING};

/// Spins of the status register to wait for the second response of a Pause.
///
/// A spin of the polled readers costs about 24 system clocks on the console, so
/// this is about 24 million clocks (0.7 s): over six times the slowest completion
/// measured (3.84 million out of a settled single-speed read), and still bounded so
/// a dead drive cannot hang a loader. Even at an optimistic 5 clocks a spin it is
/// 5 million. Reaching the bound is a failure path; the normal wait ends at INT2.
pub const COMPLETE_SPINS: u32 = 1_000_000;

/// Times the status is asked, one command each (about 20,000 clocks), when the
/// second response did not come: about 20 million clocks (0.6 s).
pub const STATUS_POLLS: u32 = 1_000;

/// Spins to wait for the first data sector of a read whose Pause was refused,
/// i.e. for its seek to end: the slowest seek measured is 310 ms, about 440,000
/// spins at 24 clocks each.
pub const SEEK_SPINS: u32 = 1_000_000;

/// Pauses sent at most, counting the first. A refusal waits the seek out and
/// retries; three is enough for a seek that began between two tries.
pub const ATTEMPTS: u32 = 3;

/// The first answer to a Pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseAnswer {
    /// INT3: the drive took the command; INT2 follows when it has stopped.
    Acknowledged,
    /// INT5: refused, the head is mid-seek. The read carries on.
    Refused,
    /// No answer within the command's own wait.
    Silent,
}

/// The wait for the second answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    /// INT2: the drive has stopped.
    Arrived,
    /// INT5 instead of INT2.
    Error,
    /// Neither within the bound.
    TimedOut,
}

/// Why a Pause could not be proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseFailure {
    /// The Pause was never acknowledged (no drive, tray open, dead controller).
    NoAnswer,
    /// Refused with INT5 on every try, or the second response was INT5.
    Refused,
    /// Acknowledged, but neither INT2 nor a status without an active bit came
    /// within the bounds: the drive is still reading, seeking or playing.
    NeverStopped,
}

impl PauseFailure {
    /// A stable one-byte code for [`SectorReader::diagnostics`](super::reader::SectorReader::diagnostics)
    /// and on-screen reports.
    pub const fn code(self) -> u8 {
        match self {
            PauseFailure::NoAnswer => 1,
            PauseFailure::Refused => 2,
            PauseFailure::NeverStopped => 3,
        }
    }
}

/// What [`pause_until_stopped`] needs from the controller. Each method is one
/// polled controller step; the implementor owns masking and acknowledging.
pub trait PauseDrive {
    /// Send Pause and wait for its first response.
    fn send_pause(&mut self) -> PauseAnswer;
    /// Wait up to `spins` for the second response. Data sectors still arriving
    /// from the interrupted read are consumed and acknowledged meanwhile.
    fn wait_completion(&mut self, spins: u32) -> Completion;
    /// GetStat: the status byte, `None` when it did not answer. Cancels a
    /// second response still pending.
    fn status(&mut self) -> Option<u8>;
    /// After a refusal: wait up to `spins` for the read to deliver its first
    /// sector (consuming it), which ends the seek. `false` if none came.
    fn wait_seek_end(&mut self, spins: u32) -> bool;
}

/// Pause the drive and return only when it has stopped.
///
/// `Ok` means the second response arrived, or the status reported no reading,
/// seeking or playing bit. The drive is then idle and spun up.
pub fn pause_until_stopped(drive: &mut impl PauseDrive) -> Result<(), PauseFailure> {
    let mut refused = false;
    for _ in 0..ATTEMPTS {
        match drive.send_pause() {
            PauseAnswer::Silent => return Err(PauseFailure::NoAnswer),
            PauseAnswer::Refused => {
                refused = true;
                // The read carries on through the seek; send again once it is over.
                drive.wait_seek_end(SEEK_SPINS);
            }
            PauseAnswer::Acknowledged => match drive.wait_completion(COMPLETE_SPINS) {
                Completion::Arrived => return Ok(()),
                Completion::Error => refused = true,
                Completion::TimedOut => {
                    // INT2 never came (lost, or later than any measured). Ask.
                    return if wait_until_idle(drive) {
                        Ok(())
                    } else {
                        Err(PauseFailure::NeverStopped)
                    };
                }
            },
        }
    }
    if refused {
        Err(PauseFailure::Refused)
    } else {
        Err(PauseFailure::NeverStopped)
    }
}

/// Status bits that mean the drive is doing something.
const ACTIVE: u8 = STAT_PLAYING | STAT_SEEKING | STAT_READING;

/// Ask for the status until no active bit is set. `false` when the polls run
/// out or the drive stops answering.
fn wait_until_idle(drive: &mut impl PauseDrive) -> bool {
    for _ in 0..STATUS_POLLS {
        match drive.status() {
            Some(status) if status & ACTIVE == 0 => return true,
            Some(_) => {}
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The console's Pause, in spins (one status read, about 24 clocks, is one
    /// spin; one GetStat is about 800 spins). The timeline starts at the
    /// first `send_pause`.
    struct Console {
        /// Spins from the acknowledge to INT2, `None` when INT2 never comes.
        completes_after: Option<u32>,
        /// Spins from the acknowledge until the status stops saying reading.
        status_clears_after: u32,
        /// Spins of a seek that refuses a Pause, counted from the first send.
        seeking_for: u32,
        seek_waits: u32,
        /// Spins since the first send.
        now: u32,
        /// Pause commands sent.
        sends: u32,
        statuses: u32,
        /// Spin at which the accepted Pause was acknowledged.
        acked_at: Option<u32>,
        /// A GetStat has cancelled the second response.
        cancelled: bool,
        silent: bool,
    }

    /// One GetStat in spins: 19,500 clocks over 24 per spin.
    const GETSTAT_SPINS: u32 = 800;

    impl Console {
        fn new(completes_after: Option<u32>) -> Self {
            Console {
                completes_after,
                status_clears_after: completes_after.unwrap_or(0),
                seeking_for: 0,
                seek_waits: 0,
                now: 0,
                sends: 0,
                statuses: 0,
                acked_at: None,
                cancelled: false,
                silent: false,
            }
        }
    }

    impl PauseDrive for Console {
        fn send_pause(&mut self) -> PauseAnswer {
            self.sends += 1;
            if self.silent {
                return PauseAnswer::Silent;
            }
            if self.now < self.seeking_for {
                self.now += 1_500; // the INT5 answer
                return PauseAnswer::Refused;
            }
            self.now += 1_400; // the acknowledge, 33,000 clocks
            self.acked_at = Some(self.now);
            self.cancelled = false;
            PauseAnswer::Acknowledged
        }

        fn wait_completion(&mut self, spins: u32) -> Completion {
            let acked = self.acked_at.expect("a wait follows an acknowledge");
            match self.completes_after {
                Some(after) if !self.cancelled && after <= spins => {
                    self.now = acked + after;
                    Completion::Arrived
                }
                _ => {
                    self.now += spins;
                    Completion::TimedOut
                }
            }
        }

        fn status(&mut self) -> Option<u8> {
            self.statuses += 1;
            self.now += GETSTAT_SPINS;
            // A command between the two responses cancels the second.
            self.cancelled = true;
            let reading = self
                .acked_at
                .is_none_or(|acked| self.now < acked + self.status_clears_after);
            Some(if reading { STAT_READING | 0x02 } else { 0x02 })
        }

        fn wait_seek_end(&mut self, spins: u32) -> bool {
            self.seek_waits += 1;
            // The first sector arrives when the seek ends, if within `spins`.
            let first_sector = self.seeking_for.saturating_add(4_700);
            if first_sector <= self.now.saturating_add(spins) {
                self.now = self.now.max(first_sector);
                true
            } else {
                self.now += spins;
                false
            }
        }
    }

    /// 3.84 million clocks (the slowest, single speed, settled head) in spins.
    const SLOWEST: u32 = 3_840_000 / 24;

    #[test]
    fn the_bounds_cover_the_slowest_measured_completion_with_margin() {
        // 24 clocks a spin on the console; even at a quarter of that it holds.
        assert!(u64::from(COMPLETE_SPINS) * 24 > 6 * 3_840_000);
        assert!(u64::from(COMPLETE_SPINS) * 5 >= 5_000_000);
        // The wait the three games used (16,384 spins) was a tenth of the worst case.
        const { assert!(16_384 < SLOWEST) };
        // Longer than the slowest seek measured (310 ms = 10.5 million clocks).
        assert!(u64::from(SEEK_SPINS) * 24 > 10_500_000);
    }

    #[test]
    fn a_pause_that_completes_at_the_slowest_measured_time_is_waited_for() {
        let mut console = Console::new(Some(SLOWEST));
        assert_eq!(pause_until_stopped(&mut console), Ok(()));
        assert_eq!(console.sends, 1);
        assert_eq!(console.statuses, 0, "no GetStat: it would cancel INT2");
    }

    #[test]
    fn the_old_sixteen_thousand_spin_wait_gives_up_on_every_measured_completion() {
        // `SectorReader::stop` waits 16,384 spins for INT2 and goes on: every
        // completion measured on the console is later than that, so a caller
        // that needs the drive stopped cannot take that wait as the answer.
        for clocks in [
            1_220_000u32,
            1_420_000,
            1_690_000,
            1_930_000,
            2_420_000,
            3_840_000,
        ] {
            let spins = clocks / 24;
            assert!(spins > 16_384, "{clocks} clocks");
            let mut console = Console::new(Some(spins));
            assert_eq!(console.send_pause(), PauseAnswer::Acknowledged);
            assert_eq!(console.wait_completion(16_384), Completion::TimedOut);
        }
    }

    #[test]
    fn an_idle_drive_stops_at_once() {
        let mut console = Console::new(Some(340));
        assert_eq!(pause_until_stopped(&mut console), Ok(()));
    }

    #[test]
    fn a_lost_second_response_is_settled_by_the_status() {
        let mut console = Console::new(None);
        // Still reading when the wait for INT2 gives up, stopped a few asks later.
        console.status_clears_after = COMPLETE_SPINS + 5 * GETSTAT_SPINS;
        assert_eq!(pause_until_stopped(&mut console), Ok(()));
        assert!(
            console.statuses >= 5,
            "it asked until the reading bit cleared"
        );
        assert_eq!(console.sends, 1, "the Pause is not sent again");
    }

    #[test]
    fn a_drive_that_never_stops_is_reported_not_waited_for_forever() {
        let mut console = Console::new(None);
        console.status_clears_after = u32::MAX / 2;
        assert_eq!(
            pause_until_stopped(&mut console),
            Err(PauseFailure::NeverStopped)
        );
        assert_eq!(console.statuses, STATUS_POLLS);
    }

    #[test]
    fn a_pause_refused_during_the_seek_is_sent_again_after_it() {
        let mut console = Console::new(Some(SLOWEST));
        console.seeking_for = 10_500_000 / 24; // 310 ms, the slowest seek
        assert_eq!(pause_until_stopped(&mut console), Ok(()));
        assert_eq!(console.sends, 2, "refused once, then accepted");
        assert_eq!(
            console.seek_waits, 1,
            "it waited for the first sector, not the status"
        );
        assert_eq!(console.statuses, 0);
    }

    #[test]
    fn a_refusal_that_never_ends_stops_after_the_attempts() {
        let mut console = Console::new(Some(SLOWEST));
        console.seeking_for = u32::MAX / 2;
        assert_eq!(
            pause_until_stopped(&mut console),
            Err(PauseFailure::Refused)
        );
        assert_eq!(console.sends, ATTEMPTS);
    }

    #[test]
    fn a_drive_that_does_not_answer_is_not_retried() {
        let mut console = Console::new(Some(1));
        console.silent = true;
        assert_eq!(
            pause_until_stopped(&mut console),
            Err(PauseFailure::NoAnswer)
        );
        assert_eq!(console.sends, 1);
    }

    #[test]
    fn failure_codes_are_distinct_and_nonzero() {
        let codes = [
            PauseFailure::NoAnswer.code(),
            PauseFailure::Refused.code(),
            PauseFailure::NeverStopped.code(),
        ];
        assert!(codes.iter().all(|&c| c != 0));
        assert_ne!(codes[0], codes[1]);
        assert_ne!(codes[1], codes[2]);
        assert_ne!(codes[0], codes[2]);
    }
}
