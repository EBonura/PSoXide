//! CD-ROM command-response cycle delays.
//!
//! ## Provenance
//!
//! The delay constants in this module are transcribed from PCSX-Redux's
//! `core/cdrom.cc` (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later, with the
//! upstream line numbers preserved inline. PSoXide is released under
//! GPL-2.0-or-later in part to honor this derivation; see `LICENSE` and
//! `docs/license-audit.md`.

/// Canonical cycle delays for command responses. The long-operation values
/// below retain their Redux provenance, while first-response acknowledgement
/// timing is calibrated from real controller measurements.
///
/// Redux cross-references (line numbers from the upstream file):
///
/// - `AddIrqQueue(CdlID + 0x100, 20480)` -- GetID second response,
///   ~4.4 µs, observed across boot roms (L900). `CdlInit` (`0x1C`)
///   uses the separate lid/rescan path instead of a second CDROM IRQ.
/// - `AddIrqQueue(CdlReset + 0x100, 4100000)` -- Reset (`0x0A`)
///   completion. MGS polls this INT2 before it starts issuing reads.
/// - `cdReadTime = psxClockSpeed / 75` -- one PSX CD-frame period
///   (L135). Redux schedules the first ReadN/ReadS sector at
///   `cdReadTime` in double-speed mode, then chains steady-state
///   sectors at `cdReadTime / 2` (single-speed uses 2x those delays).
/// - `scheduleCDPlayIRQ(SEEK_DONE ? 0x800 : cdReadTime * 4)` --
///   SeekL / SeekP second response (L875). If the target is already
///   seeked, quick ack; otherwise a full seek-time equivalent.
/// Typical command acknowledgement with no readable media. The CD controller
/// sub-CPU services commands from its firmware loop rather than responding in
/// the old fixed 0x800-cycle shortcut. Current hardware-oriented emulators use
/// roughly 15,000 cycles for the no-media path.
pub(super) const FIRST_RESPONSE_CYCLES: u64 = 15_000;

/// Typical acknowledgement with a disc present. The PAL PSone capture measured
/// GetStat at 30,286 cycles minimum including 257 cycles of MMIO/poll overhead,
/// yielding a 30,029-cycle controller delay. Its 48,590-cycle maximum confirms
/// the documented firmware-loop jitter; the floor is independently visible in
/// every command, while the maintenance-loop phase is modelled by the command
/// scheduler.
pub(super) const FIRST_RESPONSE_WITH_MEDIA_CYCLES: u64 = 30_029;

/// The drive firmware periodically performs a media-maintenance sweep before
/// servicing GetStat. The SCPH-9902 envelope exposes an 18,304-cycle outlier
/// once in a five-command status-poll window, while the other acknowledgements
/// stay on the ordinary floor.
pub(super) const GETSTAT_MAINTENANCE_CYCLES: u64 = 18_320;

/// Short chained response used by the legacy GetID error path.
pub(super) const QUICK_SECOND_RESPONSE_CYCLES: u64 = 0x800;
pub(super) const IRQ_RESCHEDULE_CYCLES: u64 = 0x100;
pub(super) const GETID_SECOND_RESPONSE_CYCLES: u64 = 20_480;
pub(super) const RESET_SECOND_RESPONSE_CYCLES: u64 = 4_100_000;
pub(super) const SEEK_SECOND_RESPONSE_CYCLES: u64 = CD_READ_TIME * 4; // ≈ 1,806,336
pub(super) const PAUSE_COMPLETE_CYCLES_STANDBY: u64 = 7_000;
pub(super) const PAUSE_COMPLETE_CYCLES_ACTIVE: u64 = 1_000_000;
pub(super) const LID_BOOTSTRAP_CYCLES: u64 = 20_480;
pub(super) const LID_PREPARE_SPINUP_CYCLES: u64 = CD_READ_TIME * 150;
pub(super) const LID_PREPARE_SEEK_CYCLES: u64 = CD_READ_TIME * 26;

/// PSX system clock / CD frames per second. `33_868_800 / 75`.
/// Redux's `cdReadTime`.
pub(super) const CD_READ_TIME: u64 = 451_584;

/// Extra first-response latency for a command issued *while CD-DA audio is
/// playing*, added on top of the applicable first-response delay.
///
/// Unlike the rest of this module, this is NOT transcribed from Redux: it is a
/// PSoXide faithfulness model. On real hardware the CD sub-CPU is a single
/// controller; while it is streaming Red Book audio it services a new command
/// only after attending to the audio it is already decoding, so a command's
/// acknowledge is noticeably delayed. Emulators that ack every command in a
/// flat ~2048 cycles hide this, which is exactly why "poll the drive every
/// frame while music plays" looks free in emulation yet stalls (and, on a
/// missed/late poll, reseeks and kills the audio) on silicon.
///
/// Magnitude is derived from the audio-sector period ([`CD_READ_TIME`], a real
/// PSX spec) rather than a fabricated figure: a command lands within a fraction
/// of one CD frame. It is deliberately well under the cycles a generous polled
/// wait spins for (so intentional blocking handoffs like Pause-before-gameplay
/// still complete) and well over a cheap non-blocking poll's budget (so a
/// frame-rate status poll during playback correctly reads as "couldn't tell"
/// instead of stalling). Tune here if hardware measurement refines it.
pub(super) const CDDA_BUSY_RESPONSE_CYCLES: u64 = CD_READ_TIME / 4; // ≈ 112,896

/// PSX system clock. `CD_READ_TIME * 75`.
const MASTER_CLOCK: u64 = CD_READ_TIME * 75; // 33,868,800
/// One millisecond of system clock.
const MS: u64 = MASTER_CLOCK / 1000;
/// Sectors in a full 72-minute sweep, the longest travel a disc can ask for.
const MAX_SLED_LBA: u64 = 72 * 60 * 75; // 324,000

/// Cycles the head needs to reach a track `lba_diff` sectors away.
///
/// Like [`CDDA_BUSY_RESPONSE_CYCLES`], a PSoXide faithfulness model rather
/// than a Redux transcription: Redux acks Play and declares the drive playing
/// at once, which makes every "has the track finished?" poll answer correctly
/// by accident. A real drive, and DuckStation, seek first, reporting SEEKING
/// with the playing bit CLEAR for the whole journey. Guest code that reads
/// "not playing" as "track over" therefore passes in emulation and restarts
/// its music on silicon.
///
/// Three regimes, after DuckStation's `GetTicksForSeek` (`src/core/cdrom.cpp`)
/// with its logarithmic terms linearised. The curve's exact shape matters far
/// less than its order of magnitude: what a guest can observe is that a
/// neighbouring track arrives within a frame or two while a cross-disc jump
/// takes most of a second, which is the difference that changes behaviour.
///
/// Deliberately free of DuckStation's per-seek jitter. That exists to shake
/// loose games that livelock on identical seek times; determinism is worth
/// more here, since the parity suites and every headless capture depend on it.
pub(super) fn cdda_seek_cycles(lba_diff: u32) -> u64 {
    let diff = lba_diff as u64;
    if diff <= 8 {
        // Already under the head. The mech just waits for the sector to come
        // round again.
        2 * CD_READ_TIME
    } else if diff < 300 {
        50 * MS
    } else if diff < 7_200 {
        // Still no sled movement: the lens tracks across on its own.
        100 * MS
    } else {
        // Sled seek: a fixed cost to get the assembly moving, then distance.
        200 * MS + 700 * MS * diff.min(MAX_SLED_LBA) / MAX_SLED_LBA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regimes have to stay ordered and stay inside the envelope a real
    /// sled shows, or the model says nothing useful about a guest's polling.
    #[test]
    fn seek_time_grows_with_distance_and_stays_in_range() {
        let ms = |cycles: u64| cycles / MS;
        assert!(ms(cdda_seek_cycles(0)) < 30, "same sector is not a seek");
        assert!(cdda_seek_cycles(8) < cdda_seek_cycles(9));
        assert!(cdda_seek_cycles(299) < cdda_seek_cycles(300));
        assert!(cdda_seek_cycles(7_199) < cdda_seek_cycles(7_200));
        // A full-disc sweep: most of a second, and never beyond a real one.
        let sweep = ms(cdda_seek_cycles(MAX_SLED_LBA as u32));
        assert!((800..=950).contains(&sweep), "{sweep} ms for a full sweep");
        // Past the end of the disc cannot cost more than crossing all of it.
        assert_eq!(
            cdda_seek_cycles(u32::MAX),
            cdda_seek_cycles(MAX_SLED_LBA as u32)
        );
    }

    /// The case the demo disc hit: menu music at the far end of a full disc,
    /// polled twice a second. The seek has to outlast a poll interval or the
    /// emulator cannot show the bug that motivated this model.
    #[test]
    fn a_cross_disc_seek_outlasts_a_two_hertz_poll() {
        assert!(cdda_seek_cycles(290_000) > 500 * MS);
    }
}
