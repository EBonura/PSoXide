// SPDX-License-Identifier: GPL-2.0-or-later
//! Partial results on the memory card.
//!
//! The capture is only shown at the end of a run, so a run that stops partway
//! used to leave nothing. With a card in a slot, and only when L1 and R1 were
//! held as the run started (the same chord the card diagnostic asks for its
//! writes), the encoded capture so far is written to the card after every
//! area as one file, replacing the last. `VIEW SAVED RESULTS (CARD)` on the
//! menu reads it back as the usual QR pages; the capture is the same format
//! with the later areas' records not yet there.
//!
//! A card that is not formatted is left alone (this never formats), and so is
//! one without room for the file twice over (the old copy stays on the card
//! until the new one is complete).

use crate::photo::PhotoCapture;
use psx_io::controller_port::Port;
use psx_io::periph::ControllerPort;
use psx_mc::{Card, HardwareCard};
use psx_rt::tty;

const NAME: &str = "BESLES-HWTEST21";
const TITLE: &str = "HWTEST V2.1 RESULTS";
static mut BLOB: [u8; PhotoCapture::SAVED_CAP] = [0; PhotoCapture::SAVED_CAP];

fn token() -> ControllerPort {
    // SAFETY: a token is a logic guard; nothing else drives SIO0 between steps.
    unsafe { ControllerPort::steal() }
}

/// Write the capture to the first card that takes it. `true` on success.
pub(crate) fn save(capture: &PhotoCapture) -> bool {
    // SAFETY: single thread; the buffer is this module's.
    let blob = unsafe { &mut *(&raw mut BLOB) };
    let len = capture.save_blob(blob);
    let mut port = token();
    for slot in [Port::One, Port::Two] {
        let mut card = Card::new(HardwareCard::on_port(&mut port, slot));
        if !matches!(card.is_formatted(), Ok(true)) {
            continue;
        }
        // The old copy goes first if there is not room for both.
        if card.free_blocks().unwrap_or(0) < 4 {
            let _ = card.delete(NAME);
        }
        if card.free_blocks().unwrap_or(0) < 2 {
            continue;
        }
        if card.write(NAME, TITLE, &blob[..len]).is_ok() {
            tty::println("hardware-tests: checkpoint saved to the card");
            return true;
        }
    }
    tty::println("hardware-tests: checkpoint not saved");
    false
}

/// Read the saved capture back into `capture`. `false` if no card holds one.
pub(crate) fn load(capture: &mut PhotoCapture) -> bool {
    // SAFETY: single thread; the buffer is this module's.
    let blob = unsafe { &mut *(&raw mut BLOB) };
    let mut port = token();
    for slot in [Port::One, Port::Two] {
        let mut card = Card::new(HardwareCard::on_port(&mut port, slot));
        if !matches!(card.is_formatted(), Ok(true)) {
            continue;
        }
        if let Ok(len) = card.read(NAME, blob) {
            if capture.load_saved(&blob[..len]) {
                return true;
            }
        }
    }
    false
}
