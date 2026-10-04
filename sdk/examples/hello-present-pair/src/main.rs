//! `hello-present-pair` -- whole frames through psx-rt's present queue with
//! no `unsafe` beyond taking the two static storages.
//!
//! `psx_gpu::present::PresentPair` builds `FRAMES` frames of varying cost
//! and hands each to the queue, which kicks it at VBlank once the frame
//! before it has drawn. Every frame must be kicked exactly once with no
//! stall recovery. The verdict goes to the TTY (`PRESENT-PAIR PASS ...` or
//! `PRESENT-PAIR FAIL ...`) and the screen turns green or red.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::present::{PresentPair, PresentStorage};
use psx_gpu::prim::TriGouraud;
use psx_gpu::Gpu;
use psx_rt::{interrupts, present, tty, Peripherals};

/// Frames to present.
const FRAMES: u32 = 120;
/// Most triangles in one frame; frame `f` draws `f % (TRIS + 1)`.
const TRIS: usize = 12;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
static mut A: PresentStorage<8, [TriGouraud; TRIS]> = PresentStorage::new([EMPTY; TRIS]);
static mut B: PresentStorage<8, [TriGouraud; TRIS]> = PresentStorage::new([EMPTY; TRIS]);

/// Print `label=value` in hex on the TTY.
fn print_hex(label: &str, value: u32) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = [0u8; 8];
    for (i, digit) in text.iter_mut().enumerate() {
        *digit = DIGITS[(value >> (28 - 4 * i) & 0xF) as usize];
    }
    tty::print(" ");
    tty::print(label);
    tty::print("=");
    tty::print(core::str::from_utf8(&text).unwrap_or("?"));
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    let Some(peripherals) = Peripherals::take() else {
        return;
    };
    let display = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240);
    let gpu = Gpu::new(peripherals.gpu_dma, display);
    // SAFETY: the only references ever made to A and B.
    let (a, b) = unsafe { (&mut *addr_of_mut!(A), &mut *addr_of_mut!(B)) };

    let before = present::stats();
    let buffers = DoubleBuffer::new(Resolution::R320X240);
    let mut pair = PresentPair::start(a, b, buffers, gpu);
    let start = interrupts::vblank_count();
    for f in 0..FRAMES {
        pair.present((8, 24, 8), |frame, packets| {
            let count = (f as usize) % (TRIS + 1);
            for (t, packet) in packets.iter_mut().take(count).enumerate() {
                let shade = ((t as u32 * 20 + f * 3) & 0x7F) as u8 + 0x20;
                let verts = if t & 1 == 0 {
                    [(0, 0), (319, 0), (0, 239)]
                } else {
                    [(319, 239), (0, 239), (319, 0)]
                };
                *packet = TriGouraud::new(verts, [(shade, 0, 40), (0, shade, 40), (40, 0, shade)]);
                frame.add(1 + t % 6, packet);
            }
        });
    }
    let (_storage, mut buffers, mut gpu) = pair.release();
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    let after = present::stats();
    let kicks = after.kick_count.wrapping_sub(before.kick_count);
    let recoveries = after.recovery_count.wrapping_sub(before.recovery_count);

    let pass =
        kicks == FRAMES && recoveries == 0 && vblanks >= FRAMES && interrupts::fault_count() == 0;
    tty::print(if pass {
        "PRESENT-PAIR PASS"
    } else {
        "PRESENT-PAIR FAIL"
    });
    print_hex("frames", FRAMES);
    print_hex("kicks", kicks);
    print_hex("recoveries", recoveries);
    print_hex("vblanks", vblanks);
    tty::println("");

    let verdict = if pass { (40, 160, 60) } else { (180, 40, 40) };
    loop {
        buffers.clear(&mut gpu, verdict);
        gpu.wait_idle();
        interrupts::wait_vblank();
        buffers.swap(&mut gpu);
    }
}
