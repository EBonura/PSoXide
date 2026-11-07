// SPDX-License-Identifier: GPL-2.0-or-later
//! Small pieces the v2.4 measurements share: a 32-bit system clock, bounded
//! frame waits, and the GPU helpers that never trust the GPU to answer.
//!
//! Every wait here is a loop with a counter; a wait that runs out reports it
//! to the caller, which records it.

use crate::bounds;
use psx_io::gpu as gpu_io;
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;

/// GPUSTAT bits the measurements read.
pub(crate) const STAT_IRQ1: u32 = 1 << 24;
pub(crate) const STAT_VRAM_READY: u32 = 1 << 27;

/// Timer 2 on the system clock, extended to 32 bits in software. It must be
/// read at least once every 65,536 cycles (about 1.9 ms) or a wrap is lost;
/// every loop that uses it reads it far more often than that.
pub(crate) struct Clock32 {
    last: u16,
    wraps: u32,
}

impl Clock32 {
    pub(crate) fn start() -> Self {
        timers::set_mode(Timer::Timer2, 0);
        timers::set_counter(Timer::Timer2, 0);
        Self { last: 0, wraps: 0 }
    }

    pub(crate) fn now(&mut self) -> u32 {
        let count = timers::counter(Timer::Timer2);
        if count < self.last {
            self.wraps += 1;
        }
        self.last = count;
        (self.wraps << 16) | count as u32
    }
}

/// Wait for the next VBlank, counted. False when it did not come.
pub(crate) fn frame() -> bool {
    interrupts::try_wait_vblank(bounds::scale(4_000_000))
}

/// Spin about `cycles` system-clock cycles, counted as well as timed so a
/// stopped timer cannot hang it.
pub(crate) fn spin_cycles(cycles: u32) {
    let mut clock = Clock32::start();
    let mut guard = bounds::scale(cycles / 4 + 16);
    while clock.now() < cycles && guard > 0 {
        guard -= 1;
    }
}

pub(crate) fn field() -> u32 {
    (gpu_io::status().bits() >> 31) & 1
}

/// Acknowledge a stale GPU interrupt and let the GP1 write settle.
pub(crate) fn ack_gpu_irq() {
    gpu_io::write_display_control(0x0200_0000);
    for _ in 0..32 {
        let _ = gpu_io::status();
    }
}

/// Everything sent to GP0 so far has been drawn: a GP0(1Fh) interrupt request
/// is taken in order with the drawing, so GPUSTAT bit 24 means the queue is
/// empty. False when it never rose within the bound.
pub(crate) fn drain() -> bool {
    ack_gpu_irq();
    gpu_io::write_command(0x1F00_0000);
    let mut spins = bounds::scale(2_000_000);
    while gpu_io::status().bits() & STAT_IRQ1 == 0 {
        if spins == 0 {
            return false;
        }
        spins -= 1;
    }
    ack_gpu_irq();
    true
}

/// Send a command packet word by word.
pub(crate) fn send(words: &[u32]) {
    for word in words {
        gpu_io::write_command(*word);
    }
}

pub(crate) const fn xy(x: u32, y: u32) -> u32 {
    (y << 16) | (x & 0xFFFF)
}

/// Upload `words` data words as a `w` x `h` rectangle (two 15bpp pixels a
/// word) at `(x, y)`: `w * h` must be `2 * words.len()`.
pub(crate) fn upload(x: u32, y: u32, w: u32, h: u32, word: u32, words: u32) {
    gpu_io::wait_command_ready();
    send(&[0xA000_0000, xy(x, y), xy(w, h)]);
    for _ in 0..words {
        gpu_io::write_command(word);
    }
}

/// Read `w` x `h` pixels at `(x, y)` and call `each` with every data word.
/// False when the GPU never said it had data.
pub(crate) fn read_rect(x: u32, y: u32, w: u32, h: u32, mut each: impl FnMut(u32)) -> bool {
    gpu_io::wait_command_ready();
    send(&[0xC000_0000, xy(x, y), xy(w, h)]);
    let mut spins = bounds::scale(200_000);
    while gpu_io::status().bits() & STAT_VRAM_READY == 0 {
        if spins == 0 {
            return false;
        }
        spins -= 1;
    }
    for _ in 0..(w * h).div_ceil(2) {
        each(gpu_io::read_data());
    }
    true
}

/// Reset the GPU's command parser (GP1 01h): used to leave a transfer that a
/// measurement cut short, never in the middle of one that is wanted.
pub(crate) fn reset_command_buffer() {
    gpu_io::write_display_control(0x0100_0000);
    for _ in 0..64 {
        let _ = gpu_io::status();
    }
}

/// The drawing environment every measurement starts from: draw area the whole
/// of VRAM, no offset, no mask, texture window off, `e1` as the draw mode.
pub(crate) fn environment(e1: u32) {
    gpu_io::wait_command_ready();
    send(&[
        0xE100_0000 | e1,
        0xE200_0000,
        0xE300_0000,
        0xE400_0000 | 1023 | (511 << 10),
        0xE500_0000,
        0xE600_0000,
    ]);
}
