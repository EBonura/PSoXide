// SPDX-License-Identifier: GPL-2.0-or-later
//! GP0(A0h) image uploads carried inside GPU linked-list nodes (case `0xEA`).
//!
//! The grid runtime uploads a streamed sky into VRAM from the frame's own
//! GPU list: each atlas row is one GP0(A0h) image transfer (three setup words
//! and 192 data words) chopped into nodes of at most 16 payload words, the
//! most a console takes while it draws, followed by a texture-cache flush
//! node. Nothing in the suite ran an image transfer through a list, so that
//! path rested on the emulator alone.
//!
//! The case poisons a 384 x 4 halfword VRAM block with a fill, uploads four
//! rows through such a list on channel 2, then reads the block back with
//! GP0(C0h) and compares it word for word with the data it sent. Every row
//! halfword has bit 15 set and the poison does not, so a row that never
//! arrived cannot pass by coincidence. Every wait is bounded: a stuck
//! channel or GPU ends the case with a flag in the observed value, never a
//! hang.
//!
//! Observed value: the number of readback words that differ (0 when the
//! upload is exact), plus [`DMA_STUCK`] when channel 2 was still busy at the
//! bound, [`GPU_STUCK`] when the GPU never became ready after the walk, and
//! [`READBACK_SHORT`] when the readback stopped early.

use core::ptr::{addr_of_mut, write_volatile};

use psx_io::gpu as gpu_io;
use psx_io::{dma, irq};

use crate::{IrqGuard, TestResult};

/// Rows uploaded.
const ROWS: usize = 4;
/// Halfword width of a row: the cube-sky atlas row (1536 texels at 4 bits).
const ROW_PIXELS: u16 = 384;
/// Data words in a row.
const ROW_WORDS: usize = ROW_PIXELS as usize / 2;
/// GP0(A0h) setup words ahead of a transfer's data.
const HEADER_WORDS: usize = 3;
/// Payload words one node may carry.
const NODE_WORDS: usize = 16;
/// Top-left of the block. Clear of the 320x240 pair the suite displays.
const ORIGIN_X: u16 = 640;
const ORIGIN_Y: u16 = 256;

/// Nodes a row takes.
const ROW_NODES: usize = (HEADER_WORDS + ROW_WORDS).div_ceil(NODE_WORDS);
/// Nodes in the list: every row, then the cache flush.
const NODES: usize = ROWS * ROW_NODES + 1;
/// Words in the list: a header a node, every payload word.
const LIST_WORDS: usize = NODES + ROWS * (HEADER_WORDS + ROW_WORDS) + 1;

const _: () = assert!(NODE_WORDS <= 16, "a node over 16 words freezes a console");

static mut LIST: [u32; LIST_WORDS] = [0; LIST_WORDS];

const KICK: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;
const GPUSTAT_CMD_READY: u32 = 1 << 26;
const GPUSTAT_VRAM_READY: u32 = 1 << 27;
const GPUSTAT_DMA_READY: u32 = 1 << 28;
/// Polls of channel 2's busy flag before the walk is called stuck.
const DMA_POLLS: u32 = 4_000_000;
/// Polls of one GPUSTAT bit before it is called stuck.
const GPU_POLLS: u32 = 1_000_000;

/// Observed-value flag: channel 2 had not finished at the poll bound.
pub(crate) const DMA_STUCK: u32 = 1 << 16;
/// Observed-value flag: the GPU did not report ready after the walk.
pub(crate) const GPU_STUCK: u32 = 1 << 17;
/// Observed-value flag: the readback stopped before all words arrived.
pub(crate) const READBACK_SHORT: u32 = 1 << 18;

/// The data word `index` of row `row`. Both halfwords carry bit 15, which the
/// poison fill lacks.
fn data_word(row: usize, index: usize) -> u32 {
    let seed = ((row as u32) << 16) ^ (index as u32);
    let mixed = seed.wrapping_mul(0x9E37_79B9) ^ (seed << 7) ^ 0x1234_5678;
    mixed | 0x8000_8000
}

/// Put one node at `at` and return the index after it.
///
/// # Safety
/// `list` must be [`LIST`]'s base and `at + 1 + words.len()` within it.
unsafe fn put_node(list: *mut u32, at: usize, words: &[u32], last: bool) -> usize {
    let next = if last {
        0x00FF_FFFF
    } else {
        list.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF
    };
    write_volatile(list.add(at), ((words.len() as u32) << 24) | next);
    for (offset, word) in words.iter().enumerate() {
        write_volatile(list.add(at + 1 + offset), *word);
    }
    at + 1 + words.len()
}

/// Build the list as the sky stream does: each row's transfer chopped into
/// nodes of at most 16 words, a cache flush node last. Returns the head.
fn build_list() -> u32 {
    let list = addr_of_mut!(LIST) as *mut u32;
    let mut at = 0usize;
    for row in 0..ROWS {
        let mut words = [0u32; HEADER_WORDS + ROW_WORDS];
        words[0] = 0xA000_0000;
        words[1] = (u32::from(ORIGIN_Y + row as u16) << 16) | u32::from(ORIGIN_X);
        words[2] = (1 << 16) | u32::from(ROW_PIXELS);
        for (index, word) in words[HEADER_WORDS..].iter_mut().enumerate() {
            *word = data_word(row, index);
        }
        for node in words.chunks(NODE_WORDS) {
            // SAFETY: LIST holds a header and the payload of every node.
            at = unsafe { put_node(list, at, node, false) };
        }
    }
    // GP0(01h): clear the texture cache, the flush the sky stream ends with.
    // SAFETY: as above; the last node is the flush's.
    unsafe { put_node(list, at, &[0x0100_0000], true) };
    list as u32
}

/// Spin until `GPUSTAT & bit` is set, at most [`GPU_POLLS`] reads.
fn wait_status(bit: u32) -> bool {
    for _ in 0..GPU_POLLS {
        if gpu_io::status().bits() & bit != 0 {
            return true;
        }
    }
    false
}

/// Fill the block with the poison pixel, then fence on the command port.
fn poison_block() -> bool {
    gpu_io::wait_command_ready();
    // Mask bits off: the upload must not be filtered by a stale E6.
    gpu_io::write_command(0xE600_0000);
    gpu_io::write_command(0x0200_00FF); // fill red: pixel 0x001F
    gpu_io::write_command((u32::from(ORIGIN_Y) << 16) | u32::from(ORIGIN_X));
    gpu_io::write_command(((ROWS as u32) << 16) | u32::from(ROW_PIXELS));
    wait_status(GPUSTAT_CMD_READY)
}

/// Walk the list on channel 2. Returns the observed flags so far.
fn upload() -> u32 {
    let head = build_list();
    let old_direction = (gpu_io::status().bits() >> 29) & 3;
    gpu_io::write_display_control(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    let mut flags = 0;
    {
        let _guard = IrqGuard::mask();
        // SAFETY: silicon probe: the walk reads only LIST, which stays live
        // and untouched until the channel is idle or aborted below.
        unsafe {
            dma::raw::set_address(dma::Channel::Gpu, head);
            dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
            dma::raw::set_control(dma::Channel::Gpu, KICK);
        }
        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Gpu) && polls < DMA_POLLS {
            polls += 1;
        }
        if dma::is_busy(dma::Channel::Gpu) {
            dma::abort(dma::Channel::Gpu);
            gpu_io::write_display_control(0x0100_0000);
            flags |= DMA_STUCK;
        }
    }
    if !wait_status(GPUSTAT_CMD_READY) || !wait_status(GPUSTAT_DMA_READY) {
        flags |= GPU_STUCK;
    }
    irq::acknowledge(1 << psx_hw::irq::source::GPU);
    gpu_io::write_display_control(0x0400_0000 | old_direction);
    flags
}

/// Read the block back with GP0(C0h) and count the words that differ from
/// what [`build_list`] sent.
fn mismatches() -> u32 {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xC000_0000);
    gpu_io::write_command((u32::from(ORIGIN_Y) << 16) | u32::from(ORIGIN_X));
    gpu_io::write_command(((ROWS as u32) << 16) | u32::from(ROW_PIXELS));
    let mut wrong = 0u32;
    for row in 0..ROWS {
        for index in 0..ROW_WORDS {
            if !wait_status(GPUSTAT_VRAM_READY) {
                let unread = ROWS * ROW_WORDS - (row * ROW_WORDS + index);
                return wrong + unread as u32 + READBACK_SHORT;
            }
            if gpu_io::read_data() != data_word(row, index) {
                wrong += 1;
            }
        }
    }
    wrong
}

/// `0xEA`: four rows uploaded through 16-word list nodes read back exactly.
pub(crate) fn test_list_upload() -> TestResult {
    if !poison_block() {
        return TestResult::fail(0, GPU_STUCK, "gpu stuck before");
    }
    let flags = upload();
    let observed = mismatches() | flags;
    if observed == 0 {
        TestResult::pass(0, 0, "rows exact")
    } else if flags & DMA_STUCK != 0 {
        TestResult::fail(0, observed, "walk stuck")
    } else if flags & GPU_STUCK != 0 {
        TestResult::fail(0, observed, "gpu stuck after")
    } else if observed & READBACK_SHORT != 0 {
        TestResult::fail(0, observed, "readback short")
    } else {
        TestResult::fail(0, observed, "words differ")
    }
}
