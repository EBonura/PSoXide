// SPDX-License-Identifier: GPL-2.0-or-later
//! FMV console test (MAIN MENU, last row): the SDK's `hello-fmv` player, run
//! in place.
//!
//! The disc carries `MOVIE.STR`, a 75 s synthetic 320x240 15 fps STR at the
//! full double-speed sector budget with interleaved XA stereo beeps. Every
//! video sector carries its ordinal and a checksum, so the player counts
//! skipped sectors (LOST) apart from corrupt ones (BAD) while an overlay shows
//! the running counts, and a PASS/FAIL summary screen closes the run. The
//! player, its overlay and its pass criteria are the SDK's; this module only
//! hands the machine over, waits on the summary, and turns the outcome into
//! timing-block records so the result also travels in the QR capture.
//!
//! The stream reads through the SDK's polled PIO path at double speed, the
//! same path the SectorReader-based games use, for 9,826 video sectors.

use crate::{TimingRecord, TIMING_RECORD_COUNT, TIMING_RECORD_UNUSED};
use hello_fmv::Outcome;
use psx_engine::button;
use psx_font::FontAtlas;
use psx_rt::interrupts;

/// First of the timing-block ids that carry the last FMV run. The triples do
/// not hold min/median/max: tools/hwtest-report.py names each field.
pub(crate) const FIRST_RECORD: u16 = 0x1F0;
/// 0x1F0 pass, good, total; 0x1F1 lost, bad, dropped; 0x1F2 drive errors,
/// decode errors, first problem LBA; 0x1F3 shown, late, VBlanks; 0x1F4
/// kilocycles per frame for bitstream, MDEC and wait; 0x1F5 last good LBA,
/// runs, setup failure code.
pub(crate) const RECORD_COUNT: usize = 6;

/// `first_err_lba` when nothing went wrong.
const NO_ERROR_LBA: u16 = 0xFFFF;

/// Setup failures in the order the player can hit them; 0 is none.
const SETUP_ERRORS: [&str; 5] = [
    "cd prepare",
    "MOVIE.STR not found",
    "cd xa mode",
    "mdec tables",
    "cd start",
];

/// The exit prompt under the summary, then block until a fresh press and its
/// release, so the press that leaves cannot also select the menu row again.
/// `font` must be in VRAM again: the player's own fonts overwrite the suite's
/// atlas.
pub(crate) fn wait_for_exit(font: &FontAtlas) {
    // The player leaves the draw area on the displayed buffer at row 0.
    font.draw_text(16, 228, "CROSS OR START: BACK TO MENU", (140, 160, 190));
    psx_gpu::draw_sync();
    let exit = |buttons: psx_pad::ButtonState| {
        buttons.is_held(button::CROSS)
            || buttons.is_held(button::START)
            || buttons.is_held(button::TRIANGLE)
    };
    // Wait out whatever was held when the run started.
    while exit(psx_pad::poll_port1().buttons) {
        interrupts::wait_vblank();
    }
    while !exit(psx_pad::poll_port1().buttons) {
        interrupts::wait_vblank();
    }
    while exit(psx_pad::poll_port1().buttons) {
        interrupts::wait_vblank();
    }
}

/// A setup failure's record code (1-based, 0 = none), as 0x1F5 carries it.
pub(crate) fn setup_error_code(what: &str) -> Option<u32> {
    SETUP_ERRORS
        .iter()
        .position(|&known| known == what)
        .map(|index| index as u32 + 1)
}

fn clamp(value: u32) -> u16 {
    value.min(0xFFFF) as u16
}

fn record(offset: u16, a: u32, b: u32, c: u32) -> TimingRecord {
    TimingRecord {
        id: FIRST_RECORD + offset,
        work: 0,
        min: clamp(a),
        med: clamp(b),
        max: clamp(c),
    }
}

/// The outcome as timing-block records.
pub(crate) fn records(outcome: &Outcome, runs: u8) -> [TimingRecord; RECORD_COUNT] {
    let setup = outcome.setup_error.and_then(setup_error_code).unwrap_or(0);
    [
        record(0, outcome.pass as u32, outcome.good, outcome.total),
        record(1, outcome.lost, outcome.bad, outcome.dropped),
        record(
            2,
            outcome.cd_errors,
            outcome.decode_errors,
            outcome.first_err_lba.map_or(NO_ERROR_LBA as u32, |lba| lba.min(0xFFFE)),
        ),
        record(3, outcome.shown, outcome.late, outcome.vblanks),
        record(4, outcome.kcyc_vlc, outcome.kcyc_mdec, outcome.kcyc_wait),
        record(5, outcome.lba, runs as u32, setup),
    ]
}

/// Put `fmv` into `slots`, replacing an earlier run's records in place or
/// taking the first unused slots. Returns false if the slots were full.
pub(crate) fn merge(
    slots: &mut [TimingRecord; TIMING_RECORD_COUNT],
    fmv: &[TimingRecord; RECORD_COUNT],
) -> bool {
    let mut all = true;
    for record in fmv {
        let slot = slots
            .iter()
            .position(|slot| slot.id == record.id)
            .or_else(|| slots.iter().position(|slot| slot.id == TIMING_RECORD_UNUSED));
        match slot {
            Some(index) => slots[index] = *record,
            None => all = false,
        }
    }
    all
}
