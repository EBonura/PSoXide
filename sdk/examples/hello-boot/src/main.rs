//! `hello-boot` -- a program on the scene core with boot-only memory (psoxide-core.ld,
//! psoxide-boot.ld, psoxide.ld, in the order CORE-ABI gives).
//!
//! The start chain is `__core_boot` (the core into its window), the boot mover (`.boot` from its
//! stored copy up to its run address), then `_start`. The splash runs from `.boot`, so it plays
//! only if the mover ran. Then the program does what its heap would: it writes over all of the
//! memory `.boot` ran from, and draws the brightness overlay the splash drew too, a generic GPU
//! call both share. If any of the code resident drawing needs had been placed in `.boot`, it is
//! gone now and the program faults. Verdict on the TTY: `hello-boot: PASS`.
//!
//! It also links the film player (psx-fmv, all of it boot-only) the way a boot program plays its
//! intro, and nothing else reads the disc through psx-io's `SectorReader`: LTO then folds the
//! player's reader into psx-io's resident reader methods, the shape Half-Life's intro links in. The
//! player looks for `FILM.STR`; these discs have none, so it reports that and goes on.
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
    static __heap_end: u32;
}

/// The film player, as a boot program calls it before its game: find the movie, play it.
fn film() {
    let Some((lba, bytes)) = psx_fmv::player::find_root_file("FILM.STR") else {
        tty::println("hello-boot: no film");
        return;
    };
    let movie = psx_fmv::player::Movie {
        lba,
        frames: bytes / (2048 * 10),
        width: 320,
        height: 240,
        max_chunks: 10,
        rle_words: 0x8000,
        xa: None,
    };
    let config = psx_fmv::player::Config {
        depth: psx_fmv::player::Depth::Rgb15,
        buffers: psx_fmv::player::DEFAULT_BUFFERS,
        screen_width: 320,
        screen_height: 240,
        latency_vblanks: 2,
        stall_vblanks: 120,
    };
    // The heap above .boot (which the player runs from), which nothing uses yet: its buffers.
    // SAFETY: link-time symbols bounding memory this program does not use otherwise.
    let memory = unsafe {
        let start = core::ptr::addr_of!(__boot_end) as *mut u32;
        let end = core::ptr::addr_of!(__heap_end) as usize;
        core::slice::from_raw_parts_mut(start, (end - start as usize) / 4)
    };
    let outcome = psx_fmv::player::play(&movie, &config, memory, &mut || false);
    tty::print("hello-boot: film frames ");
    tty::print_hex_u32(outcome.shown);
    tty::println("");
}

/// Boot-only code no resident code may call: with feature `cross-ref`, `main` does, and the link
/// must refuse it.
#[cfg(feature = "cross-ref")]
#[inline(never)]
#[link_section = ".text.boot_inner.hello_boot.inner"]
fn inner(x: u32) -> u32 {
    core::hint::black_box(x) ^ 0x5a5a
}

/// Boot-only code in a section the hazard tools would not read as code: with feature
/// `unprefixed`, the link must refuse it.
#[cfg(feature = "unprefixed")]
#[inline(never)]
#[link_section = ".boot_only.hello_boot.unprefixed"]
fn unprefixed(x: u32) -> u32 {
    core::hint::black_box(x) ^ 0xa5a5
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
    film();
    #[cfg(feature = "cross-ref")]
    tty::print_hex_u32(inner(core::hint::black_box(1)));
    #[cfg(feature = "unprefixed")]
    tty::print_hex_u32(unprefixed(core::hint::black_box(1)));
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
