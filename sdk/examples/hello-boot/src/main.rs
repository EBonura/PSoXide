//! `hello-boot` -- a program on the scene core with boot-only memory (psoxide-core.ld,
//! psoxide-boot.ld, psoxide.ld, in the order CORE-ABI gives).
//!
//! The start chain is `__core_boot` (the core into its window), the boot mover (`.boot` from its
//! stored copy up to its run address), then `_start`. The splash runs from `.boot`, so it plays
//! only if the mover ran. Then the program does what its heap would: it writes over all of the
//! memory `.boot` ran from, and draws the brightness overlay the splash drew too, a generic GPU
//! call both share. If any of the code resident drawing needs had been placed in `.boot`, it is
//! gone now and the program faults. Verdict on the TTY: `hello-boot: PASS`.
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_display::Brightness;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_pad::{Controllers, Port};
use psx_rt::tty;

extern "C" {
    static __boot_start: u32;
    static __boot_end: u32;
}

/// Boot-only code no resident code may call: with feature `cross-ref`, `main` does, and the link
/// must refuse it.
#[cfg(feature = "cross-ref")]
#[inline(never)]
#[link_section = ".text.boot_inner.hello_boot.inner"]
fn inner(x: u32) -> u32 {
    core::hint::black_box(x) ^ 0x5a5a
}

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
    #[cfg(feature = "cross-ref")]
    tty::print_hex_u32(inner(core::hint::black_box(1)));
    // What the heap does to `.boot` from here on: every word of it overwritten, with a word that
    // is no instruction (a reserved-instruction fault if anything still runs from there).
    let (start, end) = unsafe {
        // SAFETY: link-time symbols; only their addresses are taken.
        (
            core::ptr::addr_of!(__boot_start) as *mut u32,
            core::ptr::addr_of!(__boot_end) as *mut u32,
        )
    };
    let mut at = start;
    while at < end {
        // SAFETY: psoxide-boot.ld's run range, the program's own memory, which nothing uses now.
        unsafe { core::ptr::write_volatile(at, 0xffff_ffff) };
        at = at.wrapping_add(1);
    }
    psx_rt::cache::flush_instruction_cache_range(start as u32, end as u32);
    tty::print("hello-boot: boot memory overwritten ");
    tty::print_hex_u32(start as u32);
    tty::print(" ");
    tty::print_hex_u32(end as u32);
    tty::println("");
    if let Some(overlay) = brightness.faded_overlay(Resolution::R320X240, 255) {
        gpu.draw(&overlay);
    }
    gpu.wait_idle();
    tty::println("hello-boot: PASS");
    loop {}
}
