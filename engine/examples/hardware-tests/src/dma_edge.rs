// SPDX-License-Identifier: GPL-2.0-or-later
//! Two GPU DMA edge cases, v2.4 (records `0x860`-`0x873`).
//!
//! **A list stopped mid-node** (`0x860`-`0x867`). A guest that stops channel 2
//! (a CHCR write with START clear) while a linked list walks leaves part of the
//! node it was sending somewhere. Soul Reaver does this every frame. The list
//! here is a large rectangle (the GPU is busy for a couple of milliseconds, so
//! the DMA stalls with the FIFO full), then a node holding a CPU-to-VRAM upload
//! of 48 distinct words, then a second upload node. The stop is written a fixed
//! number of cycles after the kick. Afterwards the GPU is given time to finish,
//! asked whether it is idle (a GP0(1Fh) interrupt request does not rise while
//! a command is still waiting for words), and its command parser is reset; the
//! destination rows are then read back, and the words that landed say how much
//! of the node reached the GPU and whether the walk went on to the next node.
//!
//! **Block-mode DMA into a full FIFO** (`0x868`-`0x873`). The same upload
//! carried by a request-mode (sync mode 1) transfer started right behind the
//! large rectangle, so the FIFO is full when the first block is offered.
//! Words that arrive are counted by the pixels that land; whether the channel
//! finished, and the clocks it took, are recorded. With the GPU idle first the
//! same transfer is the control.
//!

// The record constants document the layout; the code builds ids from their bases.
#![allow(dead_code)]

use crate::console_tests::record;
use crate::v24::{self, xy, Clock32};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use psx_io::dma;
use psx_io::gpu as gpu_io;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec list_abort: words_of_the_node_landed, words_of_the_next_node_landed, flags (a list stopped mid-node; 0x860 and every second after to 0x866; flags bit 0 the first node's words are a contiguous prefix, bit 1 the next node's, bit 2 the channel was busy at the stop write, bit 3 still busy after the wait, bit 4 the GPU was idle after the wait, bit 5 the destination was not clear before, bit 6 a read never answered, bits 8 to 15 the stop delay in units of 256 clocks)
pub(crate) const LIST_ABORT_RECORD: u16 = 0x860;
/// rec list_abort_progress: madr_words_at_stop, madr_words_after_wait, chcr_after_stop_low_half (the DMA address as words past the list head when the stop was written and after the GPU had time to finish; 0x861 and every second after to 0x867; the delay is in the flags of the record before)
pub(crate) const LIST_ABORT_PROGRESS_RECORD: u16 = 0x861;
/// rec block_fifo: words_landed, words_sent, flags (request-mode DMA behind a large rectangle; 0x868 and every second after to 0x872; flags bit 0 the landed words are a contiguous prefix, bit 1 the channel was still busy at the bound, bit 2 the GPU was idle afterwards, bit 3 the rectangle was sent first, bits 4 to 7 the block size in words divided by 4)
pub(crate) const BLOCK_FIFO_RECORD: u16 = 0x868;
/// rec block_fifo_time: clocks_low_half, clocks_high_half, gpustat_high_half_after (kick to channel idle, or to the bound; 0x869 and every second after to 0x873)
pub(crate) const BLOCK_FIFO_TIME_RECORD: u16 = 0x869;

/// The large rectangle: 512 x 256 flat, a couple of milliseconds of drawing.
const HEAVY_X: u32 = 512;
const HEAVY_Y: u32 = 0;
const HEAVY_W: u32 = 512;
const HEAVY_H: u32 = 256;
/// Where the uploads land: below the displayed picture.
const DEST_A: (u32, u32) = (0, 496);
const DEST_B: (u32, u32) = (0, 500);
/// Data words in the first and the second upload node.
const WORDS_A: u32 = 48;
const WORDS_B: u32 = 8;
/// Pixel values: never zero, so a word that did not land reads as clear.
const PATTERN_A: u32 = 0x0100;
const PATTERN_B: u32 = 0x0300;

const LIST_LEN: usize = 128;
static mut LIST: [u32; LIST_LEN] = [0; LIST_LEN];
const STREAM_LEN: usize = 270;
static mut STREAM: [u32; STREAM_LEN] = [0; STREAM_LEN];

const KICK_LIST: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;
/// Direction and sync mode kept, START clear: how a guest stops the channel.
const STOP_LIST: u32 = psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED;
const KICK_BLOCK: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START;

fn pixel_word(base: u32, index: u32) -> u32 {
    let pixel = base + index;
    (pixel << 16) | pixel
}

/// Build the stop-mid-node list. Returns its head.
fn build_abort_list() -> u32 {
    let list = addr_of_mut!(LIST) as *mut u32;
    let mut at = 0usize;
    let mut emit = |words: &[u32], last: bool| {
        let next = if last {
            0x00FF_FFFF
        } else {
            // SAFETY: LIST is sized for the three nodes.
            unsafe { list.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF }
        };
        // SAFETY: as above.
        unsafe {
            write_volatile(list.add(at), ((words.len() as u32) << 24) | next);
            for (offset, word) in words.iter().enumerate() {
                write_volatile(list.add(at + 1 + offset), *word);
            }
        }
        at += 1 + words.len();
    };
    emit(
        &[0x60FF_FFFF, xy(HEAVY_X, HEAVY_Y), xy(HEAVY_W, HEAVY_H)],
        false,
    );
    let mut node_a = [0u32; 3 + WORDS_A as usize];
    node_a[0] = 0xA000_0000;
    node_a[1] = xy(DEST_A.0, DEST_A.1);
    node_a[2] = xy(WORDS_A * 2, 1);
    for i in 0..WORDS_A {
        node_a[3 + i as usize] = pixel_word(PATTERN_A, i);
    }
    emit(&node_a, false);
    let mut node_b = [0u32; 3 + WORDS_B as usize];
    node_b[0] = 0xA000_0000;
    node_b[1] = xy(DEST_B.0, DEST_B.1);
    node_b[2] = xy(WORDS_B * 2, 1);
    for i in 0..WORDS_B {
        node_b[3 + i as usize] = pixel_word(PATTERN_B, i);
    }
    emit(&node_b, true);
    list as u32
}

/// Words of a row of `words` data words at `at` that match the pattern from
/// the start (the prefix), how many are non-zero, and whether the read worked.
fn landed(at: (u32, u32), base: u32, words: u32) -> (u32, u32, bool) {
    let mut index = 0u32;
    let mut prefix = 0u32;
    let mut in_prefix = true;
    let mut nonzero = 0u32;
    let ok = v24::read_rect(at.0, at.1, words * 2, 1, |word| {
        if word != 0 {
            nonzero += 1;
        }
        if in_prefix && word == pixel_word(base, index) {
            prefix += 1;
        } else {
            in_prefix = false;
        }
        index += 1;
    });
    (prefix, nonzero, ok)
}

fn clear_rows() -> bool {
    v24::upload(DEST_A.0, DEST_A.1, WORDS_A * 2, 1, 0, WORDS_A);
    v24::upload(DEST_B.0, DEST_B.1, WORDS_B * 2, 1, 0, WORDS_B);
    let a = landed(DEST_A, PATTERN_A, WORDS_A);
    let b = landed(DEST_B, PATTERN_B, WORDS_B);
    a.1 == 0 && b.1 == 0
}

fn dma_to_gpu_mode() {
    gpu_io::write_display_control(0x0400_0002);
    dma::enable_channel(dma::Channel::Gpu);
}

/// One stop-mid-node run. `delay` is the clocks between the kick and the stop
/// write; `None` lets the list finish (the control) and writes the stop after.
fn abort_run(id: u16, delay: Option<u32>, records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(id);
    v24::environment(0);
    let clear = clear_rows();
    let head = build_abort_list();
    dma_to_gpu_mode();
    v24::ack_gpu_irq();
    let base = dma::Channel::Gpu.register_base();
    // SAFETY: silicon probe: the transfer reads only the static list, writes
    // only the GPU, and is stopped by the write below before this returns.
    let (madr_stop, chcr_stop, busy_at_stop) = unsafe {
        dma::raw::set_address(dma::Channel::Gpu, head);
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
        let mut clock = Clock32::start();
        dma::raw::set_control(dma::Channel::Gpu, KICK_LIST);
        let target = delay.unwrap_or(400_000);
        let mut guard = crate::bounds::scale(200_000);
        while clock.now() < target && guard > 0 {
            guard -= 1;
            if delay.is_none() && !dma::is_busy(dma::Channel::Gpu) {
                break;
            }
        }
        let busy = dma::is_busy(dma::Channel::Gpu);
        let madr = dma::address(dma::Channel::Gpu);
        // Stop: START clear, everything else as the kick wrote it.
        dma::raw::set_control(dma::Channel::Gpu, STOP_LIST);
        for _ in 0..8 {
            let _ = read_volatile((base) as *const u32);
        }
        (madr, dma::control(dma::Channel::Gpu), busy)
    };
    // Time for the GPU to finish the rectangle and whatever it was handed.
    v24::spin_cycles(400_000);
    let madr_final = dma::address(dma::Channel::Gpu);
    let busy_after = dma::is_busy(dma::Channel::Gpu);
    // A GP0(1Fh) interrupt request is swallowed as a parameter or a pixel
    // while a command is unfinished: no interrupt means not idle.
    v24::ack_gpu_irq();
    gpu_io::write_command(0x1F00_0000);
    v24::spin_cycles(20_000);
    let idle = gpu_io::status().bits() & v24::STAT_IRQ1 != 0;
    v24::reset_command_buffer();
    dma::abort(dma::Channel::Gpu);
    gpu_io::write_display_control(0x0400_0000);
    v24::ack_gpu_irq();
    let a = landed(DEST_A, PATTERN_A, WORDS_A);
    let b = landed(DEST_B, PATTERN_B, WORDS_B);
    let flags = (a.0 == a.1) as u32
        | (((b.0 == b.1) as u32) << 1)
        | ((busy_at_stop as u32) << 2)
        | ((busy_after as u32) << 3)
        | ((idle as u32) << 4)
        | (((!clear) as u32) << 5)
        | (((!(a.2 && b.2)) as u32) << 6)
        | (((delay.unwrap_or(0xFF_FFFF) >> 8).min(0xFF)) << 8);
    let words_past = |address: u32| (address.wrapping_sub(head) & 0x00FF_FFFF) / 4;
    push_timing_record(records, next, record(id, a.1, b.1, flags));
    push_timing_record(
        records,
        next,
        record(
            id + 1,
            words_past(madr_stop),
            words_past(madr_final),
            chcr_stop & 0xFFFF,
        ),
    );
}

/// Request-mode transfer of `words` words in blocks of `block` words, behind
/// the large rectangle when `heavy`.
fn block_run(
    id: u16,
    words: u32,
    block: u32,
    heavy: bool,
    records: &mut Records,
    next: &mut usize,
) {
    crate::bounds::record_start(id);
    v24::environment(0);
    let data = words - 3;
    // Rows are 1024 pixels at most: 2 pixels a word.
    let _clear = {
        v24::upload(DEST_A.0, DEST_A.1, data * 2, 1, 0, data);
        true
    };
    let stream = addr_of_mut!(STREAM) as *mut u32;
    // SAFETY: STREAM holds up to 270 words; the largest run uses 256.
    unsafe {
        write_volatile(stream, 0xA000_0000);
        write_volatile(stream.add(1), xy(DEST_A.0, DEST_A.1));
        write_volatile(stream.add(2), xy(data * 2, 1));
        for i in 0..data {
            write_volatile(stream.add(3 + i as usize), pixel_word(PATTERN_A, i));
        }
    }
    dma_to_gpu_mode();
    v24::ack_gpu_irq();
    if heavy {
        v24::send(&[0x60FF_FFFF, xy(HEAVY_X, HEAVY_Y), xy(HEAVY_W, HEAVY_H)]);
    }
    let blocks = words / block;
    // SAFETY: silicon probe: reads the static stream, writes the GPU; the
    // wait is counted and the channel is stopped if it does not finish.
    let (cycles, busy) = unsafe {
        dma::raw::set_address(dma::Channel::Gpu, stream as u32);
        dma::raw::set_size(
            dma::Channel::Gpu,
            dma::size_blocks(block as u16, blocks as u16),
        );
        let mut clock = Clock32::start();
        dma::raw::set_control(dma::Channel::Gpu, KICK_BLOCK);
        let mut guard = crate::bounds::scale(600_000);
        while dma::is_busy(dma::Channel::Gpu) && guard > 0 {
            guard -= 1;
            let _ = clock.now();
        }
        let busy = dma::is_busy(dma::Channel::Gpu);
        (clock.now(), busy)
    };
    if busy {
        dma::abort(dma::Channel::Gpu);
    }
    // Let the GPU finish what it was given, then ask whether it is idle.
    v24::spin_cycles(400_000);
    v24::ack_gpu_irq();
    gpu_io::write_command(0x1F00_0000);
    v24::spin_cycles(20_000);
    let status = gpu_io::status().bits();
    let idle = status & v24::STAT_IRQ1 != 0;
    v24::reset_command_buffer();
    gpu_io::write_display_control(0x0400_0000);
    v24::ack_gpu_irq();
    let mut index = 0u32;
    let mut prefix = 0u32;
    let mut in_prefix = true;
    let mut nonzero = 0u32;
    let ok = v24::read_rect(DEST_A.0, DEST_A.1, data * 2, 1, |word| {
        if word != 0 {
            nonzero += 1;
        }
        if in_prefix && word == pixel_word(PATTERN_A, index) {
            prefix += 1;
        } else {
            in_prefix = false;
        }
        index += 1;
    });
    let flags = (prefix == nonzero && ok) as u32
        | ((busy as u32) << 1)
        | ((idle as u32) << 2)
        | ((heavy as u32) << 3)
        | ((block / 4) << 4);
    push_timing_record(records, next, record(id, nonzero, data, flags));
    push_timing_record(
        records,
        next,
        record(id + 1, cycles & 0xFFFF, cycles >> 16, status >> 16),
    );
}

pub(crate) fn run(records: &mut Records, next: &mut usize) {
    let mut id = LIST_ABORT_RECORD;
    for delay in [None, Some(6_000), Some(20_000), Some(60_000)] {
        abort_run(id, delay, records, next);
        id += 2;
    }
    let mut id = BLOCK_FIFO_RECORD;
    for (words, block) in [(64u32, 16u32), (256, 16), (64, 8)] {
        for heavy in [false, true] {
            block_run(id, words, block, heavy, records, next);
            id += 2;
        }
    }
    v24::environment(0);
}
