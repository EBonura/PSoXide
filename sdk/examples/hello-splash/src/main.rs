//! `hello-splash` -- plays the boot splash at every brightness step, then once more with another
//! line, and stops. A frame-identity and size harness for `psx-splash`: one call site, so the
//! image holds the one copy a program holds.
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_display::Brightness;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_pad::{Controllers, Port};

#[no_mangle]
fn main() {
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
    // The splash waits on the VBlank counter and leaves installing it to its caller.
    psx_rt::interrupts::install_vblank_counter();
    let mut pads = Controllers::new();
    for step in -5..=6 {
        let (line, level) = if step <= 5 {
            ("Built with PSoXide", step)
        } else {
            ("Rebuilt with PSoXide", 0)
        };
        psx_splash::show(&mut gpu, &mut buffers, line, Brightness::new(level), || {
            pads.poll(&mut port, Port::One).buttons
        });
    }
    psx_rt::tty::print("hello-splash: done\n");
    loop {}
}
