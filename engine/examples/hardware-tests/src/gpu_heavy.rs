// SPDX-License-Identifier: GPL-2.0-or-later
//! v2.25 GPU and cache rows from the 2026-11-20 hardware audit.
//!
//! **HEAVY FRAME WALK** (records `0xA60`-`0xA65`). The SDK waits for a channel-2
//! walk with `dma::wait_done(Gpu, DEFAULT_SPINS)`: 500,000 passes of a CHCR
//! read; when they run out it aborts the walk and resets the GPU's command
//! buffer (GP1 01h) mid-draw, silently in a shipping build. On the console
//! channel 2 stays busy while the GPU draws and textured drawing is about 4.7
//! times slower than the emulator, so whether a heavy frame reaches the bound
//! was not known. A linked list of [`NODES`] textured, CLUT-mapped 48 x 48
//! quads (one quad a node, ten words) is walked whole for three lengths, 256,
//! 512 and 1,024 quads, with the SDK's own loop counted past the bound:
//!
//! * `0xA60 + 2 * k`: passes of the wait loop `lo`, `hi`, HBlanks of the walk.
//! * `0xA61 + 2 * k`: HBlanks from the walk's end to the GPU reading idle,
//!   1 if the passes reached the SDK's 500,000 (its abort would have fired),
//!   the walk's HBlanks against eight VBlanks (2,104) in percent.
//!
//! The same list (512 quads) is the first part of every chain frame of
//! `PRESENT HEAVY FRAMES` (records `0xA90`-`0xA93`, in `present_path.rs`).
//!
//! **CACHE FLUSH, DMA WEDGED** (record `0xA6C`, a risky step). `psx_rt`'s
//! cache flush waits up to 1,000,000 polls for every DMA channel to be idle
//! and then flushes anyway (the Quake freeze of 2026-10-22 was a flush with a
//! live walk). A wedged channel is a console fact: a CD-ROM channel started
//! with no sector requested stays busy with nothing moving. The step starts
//! channel 3 that way, calls `flush_instruction_cache`, and records HBlanks of
//! the call `lo`, `hi` and flags (bit 0 channel 3 busy before the flush, bit 1
//! busy after it, bit 2 idle after the abort). A frozen screen at this step
//! (the stop panel names it) is the answer that a flush with a wedged channel
//! kills the program; the run shows a checkpoint of every page first.
//!
//! Decoder labels: `gpu_heavy_walk_N`, `gpu_heavy_walk_N_bounds`,
//! `cache_flush_dma_wedged`.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;
use psx_io::dma::{self, Channel};
use psx_io::timers::{self, Timer};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const WALK_RECORD: u16 = 0xA60;
const WEDGE_RECORD: u16 = 0xA6C;
/// Quads in the heavy list.
pub(crate) const NODES: usize = 1_024;
const NODE_WORDS: usize = 10;
const LENGTHS: [usize; 3] = [256, 512, 1_024];
/// The SDK's walk wait.
const SDK_SPINS: u32 = 500_000;
/// Passes past which this step's own loop gives up and aborts.
const CAP_SPINS: u32 = 40_000_000;
const HBLANK_CLOCK: u16 = 0x0100;
const QUAD: u32 = 48;
/// The font's texture page (x 320, 4-bit) and CLUT (320, 256).
const TPAGE: u32 = 320 / 64;
const CLUT: u32 = (320 / 16) | (256 << 6);

static mut HEAVY: [u32; NODES * NODE_WORDS] = [0; NODES * NODE_WORDS];
static mut WEDGE_BUFFER: [u32; 512] = [0; 512];

/// Build the list ending after `nodes` quads (at most [`NODES`]) and return its
/// head. Quads are spread over 320 x 240 screen coordinates; the draw offset
/// and area are the caller's.
#[optimize(size)]
#[inline(never)]
pub(crate) fn heavy_chain(nodes: usize) -> *const u32 {
    let nodes = nodes.clamp(1, NODES);
    // SAFETY: single thread; no walk reads the list while it is rebuilt (the
    // callers wait for channel 2 before they build).
    unsafe {
        let base = addr_of_mut!(HEAVY).cast::<u32>();
        for n in 0..nodes {
            let node = base.add(n * NODE_WORDS);
            let link = if n + 1 == nodes {
                0x00FF_FFFF
            } else {
                node.add(NODE_WORDS) as u32 & 0x00FF_FFFF
            };
            let x = (n as u32 * 37) % (320 - QUAD);
            let y = (n as u32 * 53) % (240 - QUAD);
            let shade = 0x60 + (n as u32 & 0x3F);
            let words = [
                0x2C00_0000 | shade << 16 | shade << 8 | shade,
                x | y << 16,
                CLUT << 16,
                (x + QUAD) | y << 16,
                TPAGE << 16 | 0x002F,
                x | (y + QUAD) << 16,
                0x2F00,
                (x + QUAD) | (y + QUAD) << 16,
                0x2F2F,
            ];
            node.write(9 << 24 | link);
            for (i, word) in words.iter().enumerate() {
                node.add(1 + i).write(*word);
            }
        }
        base.cast_const()
    }
}

fn gpu_idle() -> bool {
    let status = psx_io::gpu::status().bits();
    status & (1 << 26) != 0 && status & (1 << 28) != 0
}

/// One walk: (passes, HBlanks of the walk, HBlanks to GPU idle after).
#[optimize(size)]
#[inline(never)]
fn walk(head: *const u32) -> (u32, u32, u32) {
    psx_io::gpu::write_display_control(psx_hw::gpu::gp1::dma_direction(2));
    dma::enable_channel(Channel::Gpu);
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    // SAFETY: the list is static, built above and not touched until the walk
    // is waited out or aborted below.
    unsafe {
        dma::raw::set_address(Channel::Gpu, head as u32 & 0x00FF_FFFF);
        dma::raw::set_size(Channel::Gpu, 0);
        dma::raw::set_control(
            Channel::Gpu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START,
        );
    }
    // The SDK's loop (`dma::wait_done`): a CHCR read, the test, the count.
    let mut passes = 0u32;
    while dma::is_busy(Channel::Gpu) {
        if passes >= CAP_SPINS {
            dma::abort(Channel::Gpu);
            psx_io::gpu::write_display_control(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
            break;
        }
        passes += 1;
    }
    let lines = u32::from(timers::counter(Timer::Timer1));
    let mut guard = 0u32;
    while !gpu_idle() && guard < 4_000_000 {
        guard += 1;
    }
    let idle = u32::from(timers::counter(Timer::Timer1)).saturating_sub(lines);
    (passes, lines, idle)
}

/// `HEAVY FRAME WALK`.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run_walk(records: &mut Records, next: &mut usize) {
    probe_gpu!(gpu);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    for (k, &nodes) in LENGTHS.iter().enumerate() {
        let id = WALK_RECORD + 2 * k as u16;
        crate::bounds::record_start(id);
        let head = heavy_chain(nodes);
        let (passes, lines, idle) = walk(head);
        push_timing_record(records, next, record(id, passes & 0xFFFF, passes >> 16, lines));
        push_timing_record(
            records,
            next,
            record(id + 1, idle, u32::from(passes >= SDK_SPINS), lines * 100 / 2_104),
        );
    }
    gpu.set_draw_area((0, 0), (1023, 511));
}

/// `CACHE FLUSH, DMA WEDGED`.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run_cache_wedge(records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(WEDGE_RECORD);
    dma::enable_channel(Channel::Cd);
    // SAFETY: a static buffer the channel may write into; nothing reads it.
    unsafe {
        let buffer = addr_of_mut!(WEDGE_BUFFER).cast::<u32>();
        dma::raw::set_address(Channel::Cd, buffer as u32 & 0x00FF_FFFF);
        dma::raw::set_size(Channel::Cd, 512);
        dma::raw::set_control(Channel::Cd, psx_hw::dma::CHCR_START | psx_hw::dma::CHCR_TRIGGER);
    }
    let before = dma::is_busy(Channel::Cd);
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let vblank = psx_rt::interrupts::vblank_count();
    psx_rt::cache::flush_instruction_cache();
    let mut lines = u32::from(timers::counter(Timer::Timer1));
    // Past four seconds the 16-bit count wrapped: the VBlanks give the time.
    let vblanks = psx_rt::interrupts::vblank_count().wrapping_sub(vblank);
    if vblanks > 240 {
        lines = vblanks * 263;
    }
    let after = dma::is_busy(Channel::Cd);
    dma::abort(Channel::Cd);
    let idle = !dma::is_busy(Channel::Cd);
    let flags = u32::from(before) | u32::from(after) << 1 | u32::from(idle) << 2;
    push_timing_record(records, next, record(WEDGE_RECORD, lines & 0xFFFF, lines >> 16, flags));
}
