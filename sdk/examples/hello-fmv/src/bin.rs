// SPDX-License-Identifier: GPL-2.0-or-later
//! Standalone boot of the FMV console test: run it once and leave the
//! summary up until reset.
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_rt::interrupts;

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    let _ = hello_fmv::run();
    loop {
        interrupts::wait_vblank();
    }
}
