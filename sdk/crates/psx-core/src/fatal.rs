// SPDX-License-Identifier: GPL-2.0-or-later
//! The fatal screen: red, with the stage code drawn as eight bars (most significant bit on the
//! left, a set bit tall and white, a clear bit short and grey; GP0(02h) fills snap to 16 pixels,
//! so the bars sit on 32-pixel steps). No font, no VRAM upload, nothing
//! that depends on the program that ran before.

const GP0: u32 = 0x1f80_1810;
const GP1: u32 = 0x1f80_1814;
/// GPUSTAT bit 26: ready to receive a command word.
const READY_FOR_COMMAND: u32 = 1 << 26;

#[inline(never)]
fn gp0(word: u32) {
    // Bounded: a wedged GPU must not hang the fatal screen itself before it halts.
    let mut spins = 0x1_0000u32;
    // SAFETY: GPUSTAT is a fixed MMIO register.
    while unsafe { core::ptr::read_volatile(GP1 as *const u32) } & READY_FOR_COMMAND == 0
        && spins != 0
    {
        spins -= 1;
    }
    // SAFETY: GP0 is a fixed MMIO register.
    unsafe { core::ptr::write_volatile(GP0 as *mut u32, word) };
}

#[inline(always)]
fn gp1(word: u32) {
    // SAFETY: GP1 is a fixed MMIO register.
    unsafe { core::ptr::write_volatile(GP1 as *mut u32, word) };
}

/// A GP0(02h) fill: `colour` is 0xBBGGRR.
#[inline(never)]
fn fill(colour: u32, x: u32, y: u32, w: u32, h: u32) {
    gp0(0x0200_0000 | colour);
    gp0((y << 16) | x);
    gp0((h << 16) | w);
}

/// Paint the fatal screen for `stage`. Interrupts must be off; the caller halts after it.
#[inline(never)]
pub fn paint(stage: u32) {
    gp1(0x0000_0000); // reset: display off, FIFO cleared
    gp1(0x0800_0001); // 320 x 240, NTSC, 15-bit
    gp1(0x06c6_0260); // horizontal display range
    gp1(0x0704_0010); // vertical display range, lines 16..256
    gp1(0x0500_0000); // display starts at VRAM (0, 0)
    fill(0x00_00c0, 0, 0, 320, 240);
    let mut bit = 0;
    while bit < 8 {
        let set = stage & (0x80 >> bit) != 0;
        let (colour, top) = if set {
            (0xff_ffff, 80)
        } else {
            (0x60_6060, 140)
        };
        fill(colour, 32 + bit * 32, top, 16, 160 - top);
        bit += 1;
    }
    gp1(0x0300_0000); // display on
}
