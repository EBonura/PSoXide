//! `hello-ot` -- submit GPU primitives via an ordering table and
//! DMA channel 2 in linked-list mode, the same path a commercial
//! game uses.
//!
//! Draws three overlapping Gouraud triangles back-to-front; the
//! OT's depth sort determines the draw order. Running this
//! exercises the full commercial-game rendering pipe:
//!
//! 1. Start a frame: clear the OT (sets up the inter-slot chain).
//! 2. Write the primitive packets into storage the frame borrows.
//! 3. Prepend each into the OT at its Z slot.
//! 4. Fill a background rect.
//! 5. Submit the OT to GP0 via DMA linked-list and wait for the walk.
//! 6. VSync.
//!
//! The frame borrows the table and every packet until the walk is over,
//! so the borrow checker, not an `unsafe` block, proves the DMA never
//! reads a packet that is gone or changing.
//!
//! Each triangle drifts on a sine wave -- smooth back-and-forth,
//! no modulo snap-back. `psx-math::sincos` supplies the Q1.12
//! trig table (one LUT lookup per frame per triangle).

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::TriGouraud;
use psx_gpu::Gpu;
use psx_math::sincos;
use psx_rt::Peripherals;

/// Pixel displacement for a sine-driven drift. `phase_q12` is a
/// Q0.12 angle (one full revolution = 4096); amplitude is the
/// peak pixel displacement on either side of the centre.
#[inline]
fn drift(phase_q12: u16, amplitude_px: i16) -> i16 {
    // `sin_q12` returns Q1.12 in [-4096, 4096]. Multiply by amp
    // and shift 12 to scale back into pixels.
    ((sincos::sin_q12(phase_q12) * amplitude_px as i32) >> 12) as i16
}

#[no_mangle]
fn main() {
    let Some(peripherals) = Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));

    // The table and the packets live on `main`'s stack, which outlives
    // every frame; each frame borrows them until its walk is done.
    let mut ot = OrderingTable::<16>::new();
    let mut tris: [TriGouraud; 3] =
        core::array::from_fn(|_| TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]));

    let mut frame: u32 = 0;
    loop {
        // Each triangle gets a sine-driven offset at a different
        // frequency + phase. Angle steps are Q0.12 per frame --
        // 32/frame = ~4.2 s per rev; 48/frame = ~2.8 s. The full
        // cycle runs forever with no modulo-snap jumps.
        let phase_red = (frame.wrapping_mul(32) & 0xFFF) as u16;
        let phase_green = (frame.wrapping_mul(48).wrapping_add(1365) & 0xFFF) as u16; // +120°
        let phase_blue = (frame.wrapping_mul(40).wrapping_add(2731) & 0xFFF) as u16; // +240°

        let red_dx = drift(phase_red, 30);
        // Green moves vertically so the three axes differ.
        let green_dy = drift(phase_green, 22);
        let blue_dx = drift(phase_blue, 34);

        // Three overlapping triangles at different depth slots.
        // Slot 0 = front, slot 15 = back (ordering-table walker
        // visits slot N-1 first, slot 0 last → later = on top).
        // Vertex coordinates are in draw-offset-relative space,
        // which `DoubleBuffer::swap` keeps pointing at the current
        // back buffer for us.
        let red = TriGouraud::new(
            [(120 + red_dx, 60), (40 + red_dx, 160), (200 + red_dx, 160)],
            [(220, 80, 80), (80, 220, 80), (80, 80, 220)],
        );
        let green = TriGouraud::new(
            [
                (160, 40 + green_dy),
                (80, 180 + green_dy),
                (240, 160 + green_dy),
            ],
            [(80, 200, 120), (200, 80, 120), (120, 80, 200)],
        );
        let blue = TriGouraud::new(
            [
                (180 + blue_dx, 80),
                (120 + blue_dx, 200),
                (260 + blue_dx, 170),
            ],
            [(80, 160, 220), (220, 120, 80), (160, 220, 80)],
        );

        let mut ot_frame = ot.frame();
        let [back, middle, front] = &mut tris;
        *back = red;
        *middle = green;
        *front = blue;
        ot_frame.add(10, back);
        ot_frame.add(8, middle);
        ot_frame.add(6, front);

        fb.clear(&mut gpu, (0, 0, 48));
        ot_frame.submit(gpu.dma_mut());

        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);
        frame = frame.wrapping_add(1);
    }
}
