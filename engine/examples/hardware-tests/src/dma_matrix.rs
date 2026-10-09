// SPDX-License-Identifier: GPL-2.0-or-later
//! The CD (channel 3) and MDEC (channels 0 and 1) rows of the CPU/DMA
//! contention matrix, with the far end of each transfer really there.
//!
//! The other channels' rows (`35`, `36`, `9F`, `FE`, `140`-`145`, `1E0`-`1E7`)
//! need nothing on the other side. These two do: a CD transfer needs a data
//! sector sitting in the controller's buffer, and the MDEC needs a stream to
//! chew and a second channel to drain what it makes, or the first channel
//! stalls for good. So each row below first puts the far end in place:
//!
//! * CD: SetMode (speed, sector size), SetLoc to the test region, ReadN, wait
//!   for INT1 so a sector is buffered, ask for the data (BFRD) and kick DMA 3
//!   to RAM.
//! * MDEC: reset, tables, a decode command for four macroblocks, DMA 1 started
//!   first to drain, DMA 0 kicked to feed (chained: DMA 1 empties what DMA 0
//!   makes).
//!
//! The CPU loop (64 `lw` + nop, or 64 `sw` back to back, on RAM) is timed on
//! Timer 2 from the kick, once with nothing running and once while the
//! channel(s) run. The transfer's own duration is Timer 2 from the kick until
//! the channels read idle, in units of 32 clocks. Every wait is bounded (about
//! 2 million clocks, a twentieth of a second), and a timeout is recorded as
//! `0xFFFF` instead of hanging; the channels are then aborted.

use crate::console_tests::record;
use crate::{push_timing_record, IrqGuard, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;
use psx_io::dma::{self, Channel};
use psx_io::periph::Cd;
use psx_io::timers;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec cd_dma_loop: loop_idle_clocks, loop_during_clocks, transfer_clocks_div32 (ffff = timeout; lw then sw for 1x 2048, 1x 2340, 2x 2048, 2x 2340, 0x700-0x707)
const CD_DMA_RECORD: u16 = 0x700;
/// rec mdec_dma_loop: loop_idle_clocks, loop_during_clocks, transfer_clocks_div32 (ffff = timeout; lw then sw, 0x708-0x709)
const MDEC_DMA_RECORD: u16 = 0x708;

const NONE: u32 = 0xFFFF;
const CD_SPINS: u32 = 400_000;
/// The test region `cd_chain_probe.rs` and the stream cases read.
const CDTEST_LBA: u32 = 564;
const CD_WORDS_DATA: u16 = 512;
const CD_WORDS_WHOLE: u16 = 585;
const CD_KICK: u32 = psx_hw::dma::CHCR_START | psx_hw::dma::CHCR_TRIGGER;
const MACROBLOCKS: usize = 4;
/// Input words: one 32-word DMA block a macroblock (six DC-only blocks, then
/// FE00 padding the MDEC skips).
const MDEC_IN_WORDS: usize = MACROBLOCKS * 32;
/// 24-bit output: 16x16x3 bytes a macroblock, 192 words.
const MDEC_OUT_WORDS: usize = MACROBLOCKS * 192;
const MDEC_CHCR_IN: u32 = 0x0100_0201;
const MDEC_CHCR_OUT: u32 = 0x0100_0200;

static mut CD_SINK: [u32; 592] = [0; 592];
static mut MDEC_IN: [u32; MDEC_IN_WORDS] = [0; MDEC_IN_WORDS];
static mut MDEC_OUT: [u32; MDEC_OUT_WORDS] = [0; MDEC_OUT_WORDS];
static mut SCRATCH_DATA: [u32; 4] = [0; 4];

/// One kick, one loop, then wait for both channels to go idle, every wait
/// bounded. `$8` and `$15` are the two channels' register bases (the same one
/// when only one is used), `$9` the first channel's MADR, `$14` its CHCR (0 =
/// nothing runs: the idle row), `$24` the address the loop reads or writes.
/// Returns the loop's clocks in `$12` and, in `$2`, the clocks from the kick
/// to both channels idle in units of 32 (so up to 63,000 units, about 2 million
/// clocks), or -1 if that bound went by first. Timer 2 wraps every 65,536
/// clocks; the wait counts the wraps.
macro_rules! bounded_overlap {
    ($name:ident, $id:literal, $payload:expr) => {
        #[inline(never)]
        fn $name(base_a: u32, madr_a: u32, chcr_a: u32, base_b: u32, data: u32) -> (u16, u16) {
            let loop_clocks: u32;
            let total: u32;
            // SAFETY: register writes and a DMA between memory this module
            // owns; the wait below is bounded and the caller aborts the
            // channels after a timeout.
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".set noreorder\n",
                        ".balign 16\n",
                        ".word 0x34000000 | (", stringify!($id), " << 1)\n",
                        "lui $11, 0x1F80\n",
                        "ori $11, $11, 0x1120\n",
                        "sw $9, 0($8)\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        "sw $14, 8($8)\n",
                        $payload,
                        "lw $12, 0($11)\n",
                        "move $24, $zero\n",
                        "move $25, $zero\n",
                        "3:\n",
                        "lw $10, 8($8)\n",
                        "nop\n",
                        "lw $3, 8($15)\n",
                        "nop\n",
                        "or $10, $10, $3\n",
                        "srl $10, $10, 24\n",
                        "andi $10, $10, 1\n",
                        "lw $2, 0($11)\n",
                        "nop\n",
                        "sltu $3, $2, $24\n",
                        "beqz $3, 6f\n",
                        "nop\n",
                        "lui $3, 1\n",
                        "addu $25, $25, $3\n",
                        "6:\n",
                        "move $24, $2\n",
                        "beqz $10, 4f\n",
                        "nop\n",
                        "lui $3, 0x1F\n",
                        "sltu $3, $25, $3\n",
                        "bnez $3, 3b\n",
                        "nop\n",
                        "b 5f\n",
                        "addiu $2, $zero, -1\n",
                        "4:\n",
                        "addu $2, $2, $25\n",
                        "srl $2, $2, 5\n",
                        "5:\n",
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    in("$8") base_a,
                    in("$9") madr_a,
                    in("$14") chcr_a,
                    in("$15") base_b,
                    in("$24") data,
                    lateout("$25") _,
                    lateout("$2") total,
                    lateout("$3") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") loop_clocks,
                    options(nostack)
                );
            }
            (loop_clocks as u16, total as u16)
        }
    };
}

bounded_overlap!(overlap_lw, 230, ".rept 64\nlw $10, 0($24)\nnop\n.endr\n");
bounded_overlap!(overlap_sw, 231, ".rept 64\nsw $zero, 0($24)\n.endr\n");

#[derive(Copy, Clone)]
enum Loop {
    Loads,
    Stores,
}

fn run_loop(kind: Loop, base_a: u32, madr: u32, chcr: u32, base_b: u32, data: u32) -> (u16, u16) {
    let _irq = IrqGuard::mask();
    match kind {
        Loop::Loads => overlap_lw(base_a, madr, chcr, base_b, data),
        Loop::Stores => overlap_sw(base_a, madr, chcr, base_b, data),
    }
}

/// The loop on its own, warmed by a first call, from the address `data`.
fn idle_loop(kind: Loop, base: u32, data: u32) -> u16 {
    let _ = run_loop(kind, base, 0, 0, base, data);
    run_loop(kind, base, 0, 0, base, data).0
}

/// The channel's registers after a row, on the TTY, so a timeout can be told
/// from a transfer that finished.
fn debug_chcr(what: &str, channel: Channel, loop_clocks: u16, total: u16) {
    use psx_rt::tty;
    tty::print("hardware-tests: dma-matrix ");
    tty::print(what);
    tty::print(" chcr=0x");
    tty::print_hex_u32(dma::control(channel));
    tty::print(" madr=0x");
    tty::print_hex_u32(dma::address(channel));
    tty::print(" loop=");
    crate::report::tty_print_dec_u16(loop_clocks);
    tty::print(" total=");
    crate::report::tty_print_dec_u16(total);
    tty::println("");
}

fn pack(idle: u16, during: u16, transfer: u16) -> (u32, u32, u32) {
    (idle as u32, during as u32, transfer as u32)
}

// ------------------------------------------------------------------ CD

/// One CD row: a buffered sector, then DMA 3 into RAM under the loop.
fn cd_row(fast: bool, whole: bool, kind: Loop) -> (u32, u32, u32) {
    let mut cd = unsafe { Cd::steal() };
    let base = Channel::Cd.register_base();
    let sink = (&raw mut CD_SINK) as u32;
    let ram = (&raw mut SCRATCH_DATA) as u32;
    let idle = idle_loop(kind, base, ram);
    let mode = (if fast { 0x80 } else { 0 }) | (if whole { 0x20 } else { 0 });
    let words = if whole { CD_WORDS_WHOLE } else { CD_WORDS_DATA };

    // The far end: a sector in the drive's buffer.
    let ready = cd.try_set_mode(mode, CD_SPINS).is_some()
        && cd.try_set_target_lba(CDTEST_LBA, CD_SPINS).is_some()
        && cd.try_start_reading(CD_SPINS).is_some()
        && cd.try_wait_data_sector(CD_SPINS);
    if !ready {
        let _ = cd.try_pause_until_complete(CD_SPINS);
        return (idle as u32, NONE, NONE);
    }
    // Ask for the sector's data and wait for the FIFO to say it is there.
    cd.request_data();
    let mut spins = 0u32;
    while !cd.is_data_fifo_ready() && spins < 200_000 {
        spins += 1;
    }
    dma::enable_channel(Channel::Cd);
    // SAFETY: the sink is this module's, and stays untouched until the
    // channel reads idle or is aborted below.
    unsafe {
        dma::raw::set_address(Channel::Cd, sink);
        dma::raw::set_size(Channel::Cd, dma::size_words(words));
    }
    let (during, transfer) = run_loop(kind, base, sink, CD_KICK, base, ram);
    debug_chcr("cd", Channel::Cd, during, transfer);
    if transfer == 0xFFFF {
        dma::abort(Channel::Cd);
    }
    cd.clear_data_request();
    let _ = cd.try_pause_until_complete(CD_SPINS);
    pack(idle, during, transfer)
}

pub(crate) fn cd(records: &mut Records, next: &mut usize) {
    for (index, (fast, whole)) in [(false, false), (false, true), (true, false), (true, true)]
        .into_iter()
        .enumerate()
    {
        for (offset, kind) in [Loop::Loads, Loop::Stores].into_iter().enumerate() {
            let (a, b, c) = cd_row(fast, whole, kind);
            push_timing_record(
                records,
                next,
                record(CD_DMA_RECORD + (2 * index + offset) as u16, a, b, c),
            );
        }
    }
    let mut cd = unsafe { Cd::steal() };
    let _ = cd.try_set_mode(0, CD_SPINS);
}

// ---------------------------------------------------------------- MDEC

fn fill_mdec_input() {
    // SAFETY: only this module touches the buffers, with the channels idle.
    let input = unsafe { &mut *addr_of_mut!(MDEC_IN) };
    for block in 0..MACROBLOCKS {
        for word in 0..32 {
            input[block * 32 + word] = if word < 6 { 0xFE00_2040 } else { 0xFE00_FE00 };
        }
    }
}

fn mdec_row(kind: Loop) -> (u32, u32, u32) {
    let base_in = Channel::MdecIn.register_base();
    let base_out = Channel::MdecOut.register_base();
    let ram = (&raw mut SCRATCH_DATA) as u32;
    let idle = idle_loop(kind, base_in, ram);
    fill_mdec_input();
    dma::abort(Channel::MdecIn);
    dma::abort(Channel::MdecOut);
    #[allow(deprecated)]
    let ready = psx_fmv::mdec::reset() && psx_fmv::mdec::load_tables().is_some();
    if !ready {
        return (idle as u32, NONE, NONE);
    }
    let input = (&raw mut MDEC_IN) as u32;
    let output = (&raw mut MDEC_OUT) as u32;
    // SAFETY: the decode command and the buffers are this module's; the
    // channels are aborted below if they have not finished.
    unsafe {
        psx_io::write_u32(
            psx_hw::mdec::MDEC0,
            psx_hw::mdec::DECODE_24BPP | MDEC_IN_WORDS as u32,
        );
        // DMA 0 only starts once the decoder asks for data (status bit 28),
        // which takes a moment after the command; a kick before that parks.
        let mut spins = 0u32;
        while psx_io::read_u32(psx_hw::mdec::MDEC1) & psx_hw::mdec::STATUS_IN_REQUEST == 0
            && spins < 200_000
        {
            spins += 1;
        }
        dma::enable_channel(Channel::MdecIn);
        dma::enable_channel(Channel::MdecOut);
        // The drain first: block mode waits for the decoder's requests.
        dma::raw::set_address(Channel::MdecOut, output);
        dma::raw::set_size(
            Channel::MdecOut,
            dma::size_blocks(32, (MDEC_OUT_WORDS / 32) as u16),
        );
        dma::raw::set_control(Channel::MdecOut, MDEC_CHCR_OUT);
        dma::raw::set_size(Channel::MdecIn, dma::size_blocks(32, MACROBLOCKS as u16));
    }
    let (during, transfer) = run_loop(kind, base_in, input, MDEC_CHCR_IN, base_out, ram);
    debug_chcr("mdec-in", Channel::MdecIn, during, transfer);
    debug_chcr("mdec-out", Channel::MdecOut, during, transfer);
    if transfer == 0xFFFF {
        dma::abort(Channel::MdecIn);
        dma::abort(Channel::MdecOut);
    }
    // SAFETY: MDEC control write.
    unsafe { psx_io::write_u32(psx_hw::mdec::MDEC1, psx_hw::mdec::CONTROL_RESET) };
    pack(idle, during, transfer)
}

pub(crate) fn mdec(records: &mut Records, next: &mut usize) {
    timers::set_mode(timers::Timer::Timer2, 0);
    for (offset, kind) in [Loop::Loads, Loop::Stores].into_iter().enumerate() {
        let (a, b, c) = mdec_row(kind);
        push_timing_record(
            records,
            next,
            record(MDEC_DMA_RECORD + offset as u16, a, b, c),
        );
    }
}
