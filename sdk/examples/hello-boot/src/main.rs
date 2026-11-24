//! `hello-boot` -- a program on the scene core with boot-only memory (psoxide-core.ld,
//! psoxide-boot.ld, psoxide.ld, in the order CORE-ABI gives).
//!
//! The start chain is `__core_boot` (the core into its window), the boot mover (`.boot` from its
//! stored copy up to its run address), then `_start`. The splash runs from `.boot`, so it plays
//! only if the mover ran. Verdict on the TTY: `hello-boot: PASS`.
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_display::Brightness;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_pad::{Controllers, Port};
use psx_rt::tty;

#[no_mangle]
fn main() {
    tty::println("hello-boot: main");
    // On the core: the window holds this program's core image.
    if psx_core::link::window_hash() != psx_core::link::CORE_HASH {
        tty::println("hello-boot: FAIL window");
        return;
    }
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut port = peripherals.controller_port;
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut buffers = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    psx_rt::interrupts::install_vblank_counter();
    let mut pads = Controllers::new();
    // A brightness other than the default: the splash draws the overlay too.
    let brightness = Brightness::new(-2);
    psx_splash::show(
        &mut gpu,
        &mut buffers,
        "Built with PSoXide",
        brightness,
        || pads.poll(&mut port, Port::One).buttons,
    );
    tty::println("hello-boot: splash done");
    tty::println("hello-boot: PASS");
    loop {}
}
