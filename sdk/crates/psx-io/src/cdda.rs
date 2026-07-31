// SPDX-License-Identifier: GPL-2.0-or-later
//! CD-DA playback helpers: a start-handshake state machine and an
//! audio-anchored clock.
//!
//! Both are extractions of code that at least three projects grew
//! independently (gh-psx, the magikarp example, the engine's private
//! `CddaPlayer`), and both encode silicon findings that are expensive to
//! rediscover:
//!
//! * Starting CD-DA after the no-BIOS fast boot needs the drive PRIMED:
//!   SetMode -> Demute -> Play, with a GetStat drain AFTER each command
//!   (a GetStat immediately *before* a command wedges the controller
//!   instead), a cold-drive delay before the first command, and retry
//!   pacing between attempts. [`CddaStarter`] is that machine.
//! * A song clock read from the vblank counter drifts off the audio (the
//!   display and the CD run on different oscillators), while GetlocP alone
//!   is jittery and coarse (75 Hz). [`CddaClock`] anchors on GetlocP and
//!   interpolates with ticks, with a deadband, spike confirmation, and
//!   monotonic output.
//!
//! Both are poll-driven: call them once per frame/tick with your tick
//! counter. Neither takes an interrupt or blocks beyond the bounded
//! command spins.

use crate::cdrom::{self, PlayPosition};

/// Ticks to wait before the very first command: hammering CD commands while
/// the drive is still cold from boot can wedge it (observed on silicon).
pub const COLD_DRIVE_DELAY_TICKS: u32 = 45;
/// Per-command response spin budget. Generous; the drive answers far sooner.
pub const DEFAULT_COMMAND_SPINS: u32 = 131_072;
const RETRY_AFTER_OK_TICKS: u32 = 2;
const RETRY_AFTER_NAK_TICKS: u32 = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum StartStep {
    SetMode,
    Demute,
    Play,
    Done,
}

/// Poll-driven CD-DA start handshake (SetMode -> Demute -> Play).
///
/// ```ignore
/// let mut starter = CddaStarter::new();
/// starter.begin(tick); // arms the cold-drive delay
/// // then, once per tick:
/// if starter.tick(tick, track) {
///     // playback accepted this tick; e.g. clock.start(tick)
/// }
/// ```
#[derive(Copy, Clone, Debug)]
pub struct CddaStarter {
    step: StartStep,
    next_try_tick: u32,
    spins: u32,
}

impl CddaStarter {
    /// A starter in the Done state; call [`CddaStarter::begin`] to arm it.
    pub const fn new() -> Self {
        Self {
            step: StartStep::Done,
            next_try_tick: 0,
            spins: DEFAULT_COMMAND_SPINS,
        }
    }

    /// Override the per-command spin budget (emulators answer instantly;
    /// silicon wants the default).
    pub const fn with_spins(mut self, spins: u32) -> Self {
        self.spins = spins;
        self
    }

    /// Arm the handshake: first command fires [`COLD_DRIVE_DELAY_TICKS`]
    /// after `now_tick`.
    pub fn begin(&mut self, now_tick: u32) {
        self.step = StartStep::SetMode;
        self.next_try_tick = now_tick.wrapping_add(COLD_DRIVE_DELAY_TICKS);
    }

    /// Whether Play has been accepted (the handshake is finished).
    pub fn started(&self) -> bool {
        self.step == StartStep::Done
    }

    /// Which handshake step is pending, for on-screen diagnostics:
    /// 0 SetMode, 1 Demute, 2 Play, 3 Done. Note that Done means Play was
    /// ACCEPTED, not that the head has arrived; the drive may still be
    /// seeking when this reads 3.
    pub fn step_code(&self) -> u8 {
        match self.step {
            StartStep::SetMode => 0,
            StartStep::Demute => 1,
            StartStep::Play => 2,
            StartStep::Done => 3,
        }
    }

    /// Drive one attempt if due. Returns true exactly once, on the tick the
    /// Play command is accepted. Safe to keep calling afterwards (no-op).
    pub fn tick(&mut self, now_tick: u32, track: u8) -> bool {
        if self.step == StartStep::Done || now_tick.wrapping_sub(self.next_try_tick) > u32::MAX / 2
        {
            return false;
        }
        let ok = match self.step {
            StartStep::SetMode => cdrom::try_set_mode(cdrom::MODE_CDDA, self.spins).is_some(),
            StartStep::Demute => cdrom::try_demute(self.spins).is_some(),
            StartStep::Play => cdrom::try_play_track(track, self.spins).is_some(),
            StartStep::Done => unreachable!(),
        };
        // Settle the controller AFTER each command. The no-BIOS fast boot
        // leaves the drive touchy: without this drain between steps the next
        // command silently NAKs. (Issued *before* a command it wedges instead,
        // so it lives here.)
        let _ = cdrom::try_get_stat(self.spins);
        if ok {
            self.step = match self.step {
                StartStep::SetMode => StartStep::Demute,
                StartStep::Demute => StartStep::Play,
                StartStep::Play | StartStep::Done => StartStep::Done,
            };
            self.next_try_tick = now_tick.wrapping_add(RETRY_AFTER_OK_TICKS);
            self.step == StartStep::Done
        } else {
            self.next_try_tick = now_tick.wrapping_add(RETRY_AFTER_NAK_TICKS);
            false
        }
    }
}

impl Default for CddaStarter {
    fn default() -> Self {
        Self::new()
    }
}

/// Track-end detection that survives real drive behaviour.
///
/// The naive check ("GetStat shows neither playing nor seeking N times in
/// a row, so the track ended") advances spuriously on hardware: after a
/// Stop-then-Play restart the drive reports status `0x00` for one to two
/// seconds while the motor stops and re-spins, before the seek even
/// engages. An emulator answers instantly and never shows that window,
/// which is how the bug reached a burned disc.
///
/// This detector only ARMS once it has seen the drive actually playing the
/// current track; call [`rearm`](Self::rearm) whenever a new Play
/// handshake begins. Feed it every GetStat poll; it answers `true` exactly
/// once per genuine end of track.
#[derive(Copy, Clone, Debug)]
pub struct CddaEndDetector {
    armed: bool,
    quiet_polls: u8,
    threshold: u8,
}

impl CddaEndDetector {
    /// `threshold` is how many consecutive quiet polls mean "ended".
    pub const fn new(threshold: u8) -> Self {
        Self {
            armed: false,
            quiet_polls: 0,
            threshold,
        }
    }

    /// A new Play handshake is starting: disarm until the drive is seen
    /// playing again, so its stop/spin-up window cannot read as an end.
    pub fn rearm(&mut self) {
        self.armed = false;
        self.quiet_polls = 0;
    }

    /// Feed one GetStat status byte (`None` for a command timeout).
    /// Returns `true` when the current track has genuinely ended, once;
    /// the detector disarms itself for the caller's restart.
    pub fn poll(&mut self, status: Option<u8>) -> bool {
        let Some(status) = status else {
            self.quiet_polls = 0;
            return false;
        };
        if status & cdrom::STAT_PLAYING != 0 {
            self.armed = true;
            self.quiet_polls = 0;
            return false;
        }
        let quiet = status & (cdrom::STAT_PLAYING | cdrom::STAT_SEEKING) == 0;
        if !(self.armed && quiet) {
            self.quiet_polls = 0;
            return false;
        }
        self.quiet_polls += 1;
        if self.quiet_polls >= self.threshold {
            self.rearm();
            return true;
        }
        false
    }

    /// Whether the drive has been seen playing the current track.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Consecutive quiet polls so far, for debug overlays.
    pub fn quiet_polls(&self) -> u8 {
        self.quiet_polls
    }
}

/// Readings within this of the prediction are noise; ignore them.
const DEADBAND_MS: i32 = 5;
/// A single reading farther out than this is a suspected jitter spike (or a
/// real loop/seek): hold it, accept only if the next poll confirms.
const SPIKE_MS: i32 = 150;
/// Poll the drive every this many ticks (~0.5 s at 60 Hz): rare enough to be
/// cheap, often enough that interpolation drift stays under an audio frame.
const RESYNC_TICKS: u32 = 30;

/// Audio-anchored song clock: GetlocP anchor + tick interpolation.
///
/// Feed it your per-display-frame tick counter (the same one you pass to
/// [`CddaStarter`]); construct with that counter's rate (60 for NTSC, 50 for
/// PAL). Output is milliseconds into the playing track, smooth every tick,
/// monotonic (never steps backwards), and immune to single-sample GetlocP
/// jitter while still following a genuine seek or track loop.
pub struct CddaClock {
    ticks_hz: u32,
    anchor_ms: u32,
    anchor_tick: u32,
    last_now_ms: u32,
    last_poll_tick: u32,
    pending_spike_ms: Option<u32>,
    playing: bool,
}

impl CddaClock {
    /// A stopped clock counting `ticks_hz` ticks per second (60 NTSC, 50 PAL).
    pub const fn new(ticks_hz: u32) -> Self {
        Self {
            ticks_hz,
            anchor_ms: 0,
            anchor_tick: 0,
            last_now_ms: 0,
            last_poll_tick: 0,
            pending_spike_ms: None,
            playing: false,
        }
    }

    /// Call once CD-DA playback has actually started (e.g. on the tick
    /// [`CddaStarter::tick`] returns true).
    pub fn start(&mut self, now_tick: u32) {
        self.anchor_ms = 0;
        self.anchor_tick = now_tick;
        self.last_now_ms = 0;
        self.last_poll_tick = now_tick;
        self.pending_spike_ms = None;
        self.playing = true;
    }

    /// Whether [`CddaClock::start`] has been called.
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Advance and return the current song time in ms. Polls the drive at
    /// most once per [`RESYNC_TICKS`]; interpolates otherwise.
    pub fn tick(&mut self, now_tick: u32) -> u32 {
        if !self.playing {
            return 0;
        }
        if now_tick.wrapping_sub(self.last_poll_tick) >= RESYNC_TICKS {
            self.last_poll_tick = now_tick;
            if let Some(drive_ms) = poll_drive_ms() {
                self.consider_anchor(now_tick, drive_ms);
            }
        }
        let now = self.predict(now_tick);
        if now > self.last_now_ms {
            self.last_now_ms = now;
        }
        self.last_now_ms
    }

    fn predict(&self, now_tick: u32) -> u32 {
        let elapsed = now_tick.wrapping_sub(self.anchor_tick);
        self.anchor_ms + elapsed * 1000 / self.ticks_hz
    }

    /// Decide whether a fresh drive reading should move the anchor. Public
    /// within the crate so the policy is host-testable without a drive.
    fn consider_anchor(&mut self, now_tick: u32, drive_ms: u32) {
        let predicted = self.predict(now_tick);
        let delta = drive_ms as i32 - predicted as i32;
        let mag = delta.abs();

        if mag <= DEADBAND_MS {
            self.pending_spike_ms = None; // in sync; noise, ignore
            return;
        }
        if mag > SPIKE_MS {
            // Suspected jitter spike (or a loop/seek). Accept only on a
            // second consecutive reading near the same spot.
            match self.pending_spike_ms {
                Some(prev) if (drive_ms as i32 - prev as i32).abs() <= SPIKE_MS => {
                    self.set_anchor(now_tick, drive_ms);
                    // A confirmed backwards jump (track loop) must also reset
                    // the monotonic floor or the clock would freeze at the
                    // old maximum.
                    self.last_now_ms = drive_ms;
                    self.pending_spike_ms = None;
                }
                _ => self.pending_spike_ms = Some(drive_ms),
            }
            return;
        }
        // Genuine drift: re-anchor. Forward-only so a slightly-early reading
        // cannot rewind notes.
        self.pending_spike_ms = None;
        if drive_ms >= self.last_now_ms {
            self.set_anchor(now_tick, drive_ms);
        }
    }

    fn set_anchor(&mut self, now_tick: u32, drive_ms: u32) {
        self.anchor_ms = drive_ms;
        self.anchor_tick = now_tick;
    }
}

/// Poll GetlocP and convert the track-relative MSF to milliseconds. `None`
/// when the drive is not ready (short response / no disc).
fn poll_drive_ms() -> Option<u32> {
    let resp = cdrom::try_get_loc_p(16_384)?;
    PlayPosition::parse(&resp).map(|p| p.relative_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> CddaClock {
        let mut c = CddaClock::new(60);
        c.start(0);
        c
    }

    #[test]
    fn interpolates_between_anchors() {
        let mut c = started();
        c.set_anchor(0, 1000);
        assert_eq!(c.predict(30), 1500); // 30 ticks at 60 Hz = 500 ms
        assert_eq!(c.predict(60), 2000);
    }

    #[test]
    fn deadband_readings_are_ignored() {
        let mut c = started();
        c.set_anchor(0, 1000);
        c.consider_anchor(30, 1503); // predicted 1500, within 5 ms
        assert_eq!(c.anchor_ms, 1000); // unmoved
    }

    #[test]
    fn moderate_drift_reanchors_forward_only() {
        let mut c = started();
        c.set_anchor(0, 1000);
        c.last_now_ms = 1500;
        c.consider_anchor(30, 1540); // +40 ms: genuine drift
        assert_eq!((c.anchor_ms, c.anchor_tick), (1540, 30));
        // A drift reading BELOW the emitted floor must not rewind.
        c.last_now_ms = 1600;
        c.consider_anchor(33, 1560);
        assert_eq!(c.anchor_ms, 1540); // rejected
    }

    #[test]
    fn spike_needs_confirmation() {
        let mut c = started();
        c.set_anchor(0, 1000);
        c.consider_anchor(30, 9000); // wild jump: held
        assert_eq!(c.anchor_ms, 1000);
        c.consider_anchor(60, 9010); // confirmed near the same spot
        assert_eq!(c.anchor_ms, 9010);
    }

    #[test]
    fn confirmed_loop_resets_the_monotonic_floor() {
        let mut c = started();
        c.set_anchor(0, 60_000);
        c.last_now_ms = 60_000;
        c.consider_anchor(30, 100); // track looped: held
        c.consider_anchor(60, 120); // confirmed
        assert_eq!(c.anchor_ms, 120);
        assert_eq!(c.last_now_ms, 120); // floor reset, clock free to move
    }

    #[test]
    fn stopped_clock_reads_zero() {
        let mut c = CddaClock::new(60);
        assert_eq!(c.tick(100), 0);
    }

    /// The exact sequence the demo-disc debug burn photographed: after a
    /// skip the drive reports 00 00 (motor transition), then seeks, then
    /// plays. An unarmed detector must sit through all of it.
    #[test]
    fn spin_up_window_is_not_an_end_of_track() {
        let mut d = CddaEndDetector::new(3);
        d.rearm();
        for s in [0x00, 0x00, 0x00, 0x00, 0x42, 0x42, 0x82] {
            assert!(!d.poll(Some(s)), "advanced on status {s:#04x}");
        }
        assert!(d.armed());
    }

    #[test]
    fn a_real_end_fires_once_after_the_threshold() {
        let mut d = CddaEndDetector::new(3);
        d.rearm();
        assert!(!d.poll(Some(0x82)));
        assert!(!d.poll(Some(0x02)));
        assert!(!d.poll(Some(0x02)));
        assert!(d.poll(Some(0x02)), "third quiet poll after playing");
        // Disarmed for the restart: more quiet polls stay silent.
        assert!(!d.poll(Some(0x02)));
        assert!(!d.poll(Some(0x02)));
        assert!(!d.poll(Some(0x02)));
    }

    #[test]
    fn timeouts_and_seeks_reset_the_quiet_streak() {
        let mut d = CddaEndDetector::new(2);
        d.rearm();
        assert!(!d.poll(Some(0x82)));
        assert!(!d.poll(Some(0x02)));
        assert!(!d.poll(None), "timeout must not count as quiet");
        assert!(!d.poll(Some(0x02)));
        assert!(!d.poll(Some(0x42)), "a seek is activity, not quiet");
        assert!(!d.poll(Some(0x02)));
        assert!(d.poll(Some(0x02)));
    }
}
