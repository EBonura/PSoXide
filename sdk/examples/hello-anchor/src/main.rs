//! `hello-anchor` -- a title screen, a load, then a shot: the shape of a speed window anchored
//! after presses (QD Up's Start and menu presses before the pool table, then the shot).
//!
//! The program polls port 1 once a VBlank. Start begins a load of `LOAD_VBLANKS` VBlanks (20, or 50
//! with feature `long-load`); when it ends `LOADED` becomes 1 (the anchor a window names). From
//! then on it counts its frames, and the first Cross prints `hello-anchor: cross at loaded+<n>`,
//! the frame of the "shot" counted from the anchor. A tape anchored after its Start press puts that
//! Cross on the same `n` whatever the load's length; an open-loop tape from power-on does not.
#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use psx_pad::{button, Controllers, Port};
use psx_rt::tty;

/// VBlanks the load after Start takes.
#[cfg(not(feature = "long-load"))]
const LOAD_VBLANKS: u32 = 20;
#[cfg(feature = "long-load")]
const LOAD_VBLANKS: u32 = 50;

/// 1 once the load has ended: the window's anchor (`{ sym = "LOADED", op = "eq", value = 1 }`).
#[no_mangle]
static mut LOADED: u32 = 0;

#[no_mangle]
fn main() {
    tty::println("hello-anchor: title");
    psx_rt::interrupts::install_vblank_counter();
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut port = peripherals.controller_port;
    let mut pads = Controllers::new();
    let mut load_left: Option<u32> = None;
    let mut frames = 0u32;
    loop {
        psx_rt::interrupts::wait_vblank();
        let pad = pads.poll(&mut port, Port::One).buttons;
        // SAFETY: a static only this loop writes.
        let loaded = unsafe { read_volatile(addr_of!(LOADED)) } == 1;
        if loaded {
            frames += 1;
            if pad.is_held(button::CROSS) {
                tty::print("hello-anchor: cross at loaded+");
                tty::print_hex_u32(frames);
                tty::println("");
                tty::println("hello-anchor: PASS");
                psx_rt::halt();
            }
            continue;
        }
        match load_left {
            None if pad.is_held(button::START) => load_left = Some(LOAD_VBLANKS),
            Some(0) => {
                // SAFETY: as above.
                unsafe { write_volatile(addr_of_mut!(LOADED), 1) };
                tty::println("hello-anchor: loaded");
            }
            Some(n) => load_left = Some(n - 1),
            None => {}
        }
    }
}
