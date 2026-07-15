//! Video timing constants and scanline estimation helpers.
//!
//! ## Provenance
//!
//! The video-timing constants here are parity-matched against, and
//! derived from, PCSX-Redux's `psxcounters.cc`
//! (<https://github.com/grumpycoders/pcsx-redux>), Copyright (C) the
//! PCSX-Redux authors, GPL-2.0-or-later, so VBlank fires on the same
//! cycle on both sides. PSoXide is released under GPL-2.0-or-later in
//! part to honor this lineage; see `LICENSE` and `docs/license-audit.md`.

// --- Video-timing constants ---
//
// Physical beam cadence, derived from PSX-SPX's measured video clocks per
// scanline and the CPU/video-clock ratio. The old Redux-parity values divided
// by nominal 60/50 Hz and made scanlines too short; real non-interlaced PS1
// refresh is not exactly 60/50 Hz, which is directly visible to Timer 0/1.
//
//   NTSC: about 3413 video clocks/line -> 2172 CPU clocks
//   PAL : about 3406 video clocks/line -> 2167 CPU clocks
//
// `FIRST_VBLANK_CYCLE` is derived from the per-region VBlank-start
// scanline × HSync; kept as NTSC default to preserve existing parity
// tests. PAL builds call [`Bus::set_pal_mode`] before running, which
// re-seeds the VBlank scheduler and updates the tick-rate knobs.

pub(super) const HSYNC_CYCLES_NTSC: u64 = 2172;
#[allow(dead_code)]
pub(super) const HSYNC_TOTAL_NTSC: u64 = 263;
pub(super) const VBLANK_START_SCANLINE_NTSC: u64 = 243;
pub(super) const FIRST_VBLANK_CYCLE_NTSC: u64 = HSYNC_CYCLES_NTSC * VBLANK_START_SCANLINE_NTSC;
pub(super) const VBLANK_PERIOD_CYCLES_NTSC: u64 = HSYNC_CYCLES_NTSC * HSYNC_TOTAL_NTSC;

pub(super) const HSYNC_CYCLES_PAL: u64 = 2167;
#[allow(dead_code)]
pub(super) const HSYNC_TOTAL_PAL: u64 = 314;
pub(super) const VBLANK_START_SCANLINE_PAL: u64 = 256;
pub(super) const FIRST_VBLANK_CYCLE_PAL: u64 = HSYNC_CYCLES_PAL * VBLANK_START_SCANLINE_PAL;
pub(super) const VBLANK_PERIOD_CYCLES_PAL: u64 = HSYNC_CYCLES_PAL * HSYNC_TOTAL_PAL;

// NTSC first-VBlank constant kept for the default scheduler seed.
// PAL switch re-seeds via [`Bus::set_pal_mode`].
pub(super) const FIRST_VBLANK_CYCLE: u64 = FIRST_VBLANK_CYCLE_NTSC;
#[allow(dead_code)]
pub(super) const VBLANK_PERIOD_CYCLES: u64 = VBLANK_PERIOD_CYCLES_NTSC;

#[derive(Clone, Copy)]
pub(super) struct VideoTiming {
    pub(super) hsync: u64,
    pub(super) period: u64,
    pub(super) start_scanline: u64,
    pub(super) total_scanlines: u64,
}

pub(super) fn current_video_params(hsync: u64, period: u64) -> Option<VideoTiming> {
    if period == hsync.saturating_mul(HSYNC_TOTAL_NTSC) {
        Some(VideoTiming {
            hsync,
            period,
            start_scanline: VBLANK_START_SCANLINE_NTSC,
            total_scanlines: HSYNC_TOTAL_NTSC,
        })
    } else if period == hsync.saturating_mul(HSYNC_TOTAL_PAL) {
        Some(VideoTiming {
            hsync,
            period,
            start_scanline: VBLANK_START_SCANLINE_PAL,
            total_scanlines: HSYNC_TOTAL_PAL,
        })
    } else {
        None
    }
}

pub(super) fn estimate_current_scanline(
    now: u64,
    next_vblank: u64,
    period: u64,
    hsync: u64,
    start_scanline: u64,
    total_scanlines: u64,
) -> u64 {
    let previous_vblank = previous_vblank_target(now, next_vblank, period);
    let since_previous = now.saturating_sub(previous_vblank);
    (start_scanline + since_previous / hsync.max(1)) % total_scanlines.max(1)
}

pub(super) fn estimate_scanline_phase(now: u64, next_vblank: u64, period: u64, hsync: u64) -> u64 {
    let previous_vblank = previous_vblank_target(now, next_vblank, period);
    now.saturating_sub(previous_vblank) % hsync.max(1)
}

pub(super) fn previous_vblank_target(now: u64, mut next_vblank: u64, period: u64) -> u64 {
    let period = period.max(1);
    while next_vblank > now {
        next_vblank = next_vblank.saturating_sub(period);
    }
    next_vblank
}
