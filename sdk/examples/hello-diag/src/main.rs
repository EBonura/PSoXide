//! `hello-diag` -- the console stop-panel on purpose.
//!
//! Draws the `hello-tri` triangle for a while, then stops in the way the
//! `HELLO_DIAG_MODE` build variable names, so the panel (`psx_rt::diag`, feature
//! `console-diag`) can be seen on the screen it takes over:
//!
//! * `stall`: spins forever with interrupts on, no GPU and no CD activity;
//! * `fault`: executes a `BREAK`, as `panic=immediate-abort` does;
//! * `panic`: a panic with a message (this crate is built without
//!   `panic=immediate-abort`, so the message path runs);
//! * `cd`: asks the drive for a sector that is not there, to put its error
//!   counters on the panel, then stalls;
//! * anything else: keeps drawing.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

extern crate psx_rt;

use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::prim::TriGouraud;
use psx_gpu::Gpu;

const MODE: &str = match option_env!("HELLO_DIAG_MODE") {
    Some(mode) => mode,
    None => "none",
};

/// Frames drawn before the program stops itself.
const FRAMES: u16 = 120;

fn is(mode: &str) -> bool {
    let (a, b) = (MODE.as_bytes(), mode.as_bytes());
    a.len() == b.len() && {
        let mut i = 0;
        while i < a.len() && a[i] == b[i] {
            i += 1;
        }
        i == a.len()
    }
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    psx_io::diag::step(1, "DRAWING");

    let mut frame: u16 = 0;
    loop {
        fb.clear(&mut gpu, (0, 0, 64));
        let wobble = (((frame % 60) as i16) - 30).abs();
        let verts = [(160, 40 + wobble), (60, 200 - wobble), (260, 200 - wobble)];
        gpu.draw(&TriGouraud::new(
            verts,
            [(255, 64, 64), (64, 255, 64), (64, 64, 255)],
        ));
        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);
        frame = frame.wrapping_add(1);

        if frame == FRAMES {
            if is("stall") {
                psx_io::diag::step(2, "SPINNING");
                loop {
                    core::hint::spin_loop();
                }
            }
            if is("fault") {
                psx_io::diag::step(3, "BREAK");
                // SAFETY: a BREAK on purpose; psx-rt's handler halts on it.
                unsafe { core::arch::asm!("break") };
            }
            if is("panic") {
                psx_io::diag::step(4, "PANIC");
                panic!("hello-diag test panic at frame {}", frame);
            }
            if is("ll") {
                psx_io::diag::step(7, "LL NODE 41");
                // One node of 41 payload words (NOP commands): what WipEout's title emitted.
                static mut NODE: [u32; 42] = [0; 42];
                // SAFETY: a private static written once before the kick, as the DMA contract asks.
                unsafe {
                    let node = &raw mut NODE;
                    (*node)[0] = (41 << 24) | 0x00FF_FFFF;
                    psx_io::dma::wait_done(psx_io::dma::Channel::Gpu, 1_000_000);
                    psx_io::dma::start(
                        psx_io::dma::Channel::Gpu,
                        psx_io::dma::Transfer {
                            address: node as u32,
                            size: psx_io::dma::size_words(0),
                            control: psx_hw::dma::CHCR_LINKED_LIST_TO_DEVICE,
                        },
                    );
                }
                loop {
                    core::hint::spin_loop();
                }
            }
            if is("cd") {
                psx_io::diag::step(5, "CD READ");
                // A sector far past the end of the image: the drive errors or never answers.
                let mut reader = psx_io::cd::reader::SectorReader::with_cd(
                    // SAFETY: the only user of the drive in this program.
                    unsafe { psx_io::periph::Cd::steal() },
                );
                let mut sector = [0u32; psx_io::cd::reader::SECTOR_WORDS];
                if reader.prepare() && reader.start_read(5_000_000) {
                    let _ = reader.read_sector(&mut sector);
                }
                reader.stop();
                psx_io::diag::step(6, "CD DONE");
                loop {
                    core::hint::spin_loop();
                }
            }
        }
    }
}
