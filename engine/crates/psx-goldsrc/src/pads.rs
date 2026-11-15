//! The one pad reader: `psx_pad::Controllers`, polled from the title, the menus, the pause screen
//! and the play loop alike, one poll per port per frame. The first poll of a pad runs the analog
//! handshake (a few frames, once per plug-in), a garbled packet returns the last clean state, and
//! an empty port is probed once in 60 polls.

use psx_io::periph::ControllerPort;
use psx_pad::{AnalogRequirement, Controllers, PadState, Port};

static mut PADS: Controllers = Controllers::new();

/// Poll `port` once and return its latest clean state.
#[inline(never)]
pub fn poll(port: Port) -> PadState {
    // SAFETY: the game is single-threaded and polls the pad only through this function; a
    // memory-card transfer borrows the controller port through its own token and never overlaps a
    // poll. The token is zero-sized, so there is nothing to park at boot.
    unsafe {
        let mut token = ControllerPort::steal();
        *(*core::ptr::addr_of_mut!(PADS)).poll(&mut token, port)
    }
}

/// What `port` held at the last poll: analog and locked, a digital pad, or nothing.
pub fn analog(port: Port) -> AnalogRequirement {
    // SAFETY: as in `poll`.
    unsafe { (*core::ptr::addr_of!(PADS)).analog(port) }
}
