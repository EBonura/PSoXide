//! Guest side of the toolchain check: run the shared collision workload and
//! paint its hash into the framebuffer so a headless run can read it back.
//!
//! ```sh
//! tools/toolchain_check.sh
//! ```
//!
//! The hash comes from [`psx_bsp::toolchain_probe::compute_hash`], the same
//! function the native oracle calls, so a difference is a codegen difference.
//! See that module for what this caught and why the guest carries
//! `-Cllvm-args=-disable-mips-df-backward-search`.
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate psx_rt;

use psx_gpu::{Resolution, VideoMode};

/// Four-byte alignment, because the map is parsed in place from this static.
#[repr(align(4))]
struct Aligned<T: ?Sized>(T);

static MAP: &Aligned<[u8]> = &Aligned(*include_bytes!("../../editor-playtest/generated/brush_world.pxbsp"));

/// Cell width in pixels. Anything below 16 has to be drawn as a polygon:
/// GP0(02h), the VRAM fill, snaps its X to a 16-pixel boundary and rounds
/// width up to a multiple of 16, which silently smears narrow cells together
/// and returns a corrupted hash rather than an obviously broken picture.
const CELL_W: i16 = 8;

#[no_mangle]
fn main() -> ! {
    let hash = psx_bsp::toolchain_probe::compute_hash(&MAP.0);

    psx_gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    psx_gpu::set_draw_area(0, 0, 319, 239);
    psx_gpu::set_draw_offset(0, 0);
    loop {
        psx_gpu::vsync();
        psx_gpu::draw_sync();
        psx_gpu::fill_rect(0, 0, 320, 240, 0, 0, 0);
        // 32 cells, most significant bit leftmost, white for one.
        for bit in 0..32u32 {
            if hash & (1 << (31 - bit)) != 0 {
                psx_gpu::draw_rect_flat((bit as i16) * CELL_W, 0, CELL_W as u16, 20, 255, 255, 255);
            }
        }
    }
}
