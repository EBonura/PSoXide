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
///   completion. a commercial title polls this INT2 before it starts issuing reads.
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
/// GetStat at 29,771 cycles minimum including 256 cycles of MMIO/poll overhead,
/// yielding a 29,515-cycle controller delay. Its 42,661-cycle maximum confirms
/// the documented firmware-loop jitter; this deterministic core models the
/// measured floor and leaves maintenance-loop variation for a later sweep.
pub(super) const FIRST_RESPONSE_WITH_MEDIA_CYCLES: u64 = 29_515;

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
