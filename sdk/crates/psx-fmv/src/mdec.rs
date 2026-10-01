// SPDX-License-Identifier: GPL-2.0-or-later
//! MDEC driver: table upload, decode command, DMA0 in, DMA1 out.
//!
//! Decode protocol, as commercial players drive it:
//!
//! 1. [`reset`] once, then [`load_tables`] (quantization + IDCT tables).
//!    Neither assumes the MDEC reacts at once: see "Reset latency" below.
//! 2. Per frame, [`decode_start`] writes the decode command (output depth +
//!    run-length word count) to MDEC0 and kicks DMA0 with the whole
//!    run-length buffer. It does not wait: the MDEC throttles DMA0 as its
//!    output FIFO fills.
//! 3. [`read_column`] pulls one 16-pixel-wide column of decoded
//!    macroblocks over DMA1 (macroblocks come out in the order the
//!    bitstream stores them: top to bottom, then left to right), which the
//!    caller uploads to VRAM.
//! 4. [`decode_finish`] confirms DMA0 drained.
//!
//! Every wait is bounded (`psx_io::dma::wait_done`), and every kick aborts
//! the channel first, per the SDK rule for silicon DMA wedges.
//!
//! # Reset latency
//!
//! On a PAL SCPH-9002 the MDEC does not finish a reset within the next bus
//! access: the status read straight after writing the reset still shows the
//! state from before it (0x6401_0000: busy, data-in full), where the
//! SuperStation One FPGA and PSoXide already read the documented reset state.
//! Writing the DMA-request enable right behind the reset, as this driver used
//! to, left the table upload waiting on DMA0 forever on that console while
//! both of the others played the movie. So [`reset`] waits for the reset to
//! settle before enabling requests, and [`load_tables`] checks that the MDEC
//! actually raised its data-in request (status bit 28) after each command,
//! re-writing the enable if it did not, and falls back to CPU writes when
//! DMA0 still will not take the table.

use psx_io::dma::{self, Channel};

const MDEC0: u32 = 0x1F80_1820;
const MDEC1: u32 = 0x1F80_1824;

/// Decode command, 15bpp output (bits 28..27 = 3).
pub const DECODE_15BPP: u32 = 0x3800_0000;
/// Decode command, 24bpp output (bits 28..27 = 2).
pub const DECODE_24BPP: u32 = 0x3000_0000;
/// Set bit 15 on every 15bpp pixel (mask/semi-transparency bit).
pub const DECODE_STP: u32 = 0x0200_0000;

/// DMA block size the MDEC channels use, in words.
pub const DMA_BLOCK_WORDS: usize = 32;

/// Spin budget for one table upload or column transfer.
pub const DMA_SPINS: u32 = 400_000;

// CHCR: to device / from device, block sync, start.
const CHCR_IN: u32 = dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START;
const CHCR_OUT: u32 = dma::CHCR_SYNC_BLOCK | dma::CHCR_START;

/// Standard intra quantization matrix (row-major), DC entry 2.
const QUANT_ROW_MAJOR: [u8; 64] = [
    2, 16, 19, 22, 26, 27, 29, 34, //
    16, 16, 22, 24, 27, 29, 34, 37, //
    19, 22, 26, 27, 29, 34, 34, 38, //
    22, 22, 26, 27, 29, 34, 37, 40, //
    22, 26, 27, 29, 32, 35, 40, 48, //
    26, 27, 29, 32, 35, 40, 48, 58, //
    26, 27, 29, 34, 38, 46, 56, 69, //
    27, 29, 35, 38, 46, 56, 69, 83,
];

/// Zigzag position `i` to its row-major index.
const ZIGZAG_TO_ROW_MAJOR: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Luma then chroma table, zigzag order, as the 32 words command 2 takes.
pub static QUANT_WORDS: [u32; 32] = build_quant();

const fn build_quant() -> [u32; 32] {
    let mut bytes = [0u8; 128];
    let mut i = 0;
    while i < 64 {
        let q = QUANT_ROW_MAJOR[ZIGZAG_TO_ROW_MAJOR[i] as usize];
        bytes[i] = q;
        bytes[64 + i] = q;
        i += 1;
    }
    let mut words = [0u32; 32];
    let mut w = 0;
    while w < 32 {
        words[w] = u32::from_le_bytes([
            bytes[4 * w],
            bytes[4 * w + 1],
            bytes[4 * w + 2],
            bytes[4 * w + 3],
        ]);
        w += 1;
    }
    words
}

// Scaled cosines: SF0 = cos(0) * sqrt(2), SFk = cos(k*pi/16) * 2, Q14.
const SF0: i16 = 0x5A82;
const SF1: i16 = 0x7D8A;
const SF2: i16 = 0x7641;
const SF3: i16 = 0x6A6D;
const SF4: i16 = 0x5A82;
const SF5: i16 = 0x471C;
const SF6: i16 = 0x30FB;
const SF7: i16 = 0x18F8;

/// The 8x8 IDCT basis command 3 uploads.
const SCALE: [i16; 64] = [
    SF0, SF0, SF0, SF0, SF0, SF0, SF0, SF0, //
    SF1, SF3, SF5, SF7, -SF7, -SF5, -SF3, -SF1, //
    SF2, SF6, -SF6, -SF2, -SF2, -SF6, SF6, SF2, //
    SF3, -SF7, -SF1, -SF5, SF5, SF1, SF7, -SF3, //
    SF4, -SF4, -SF4, SF4, SF4, -SF4, -SF4, SF4, //
    SF5, -SF1, SF7, SF3, -SF3, -SF7, SF1, -SF5, //
    SF6, -SF2, SF2, -SF6, -SF6, SF2, -SF2, SF6, //
    SF7, -SF5, SF3, -SF1, SF1, -SF3, SF5, -SF7,
];

/// The IDCT basis as the 32 words command 3 takes.
pub static SCALE_WORDS: [u32; 32] = build_scale();

const fn build_scale() -> [u32; 32] {
    let mut words = [0u32; 32];
    let mut w = 0;
    while w < 32 {
        words[w] = (SCALE[2 * w] as u16 as u32) | ((SCALE[2 * w + 1] as u16 as u32) << 16);
        w += 1;
    }
    words
}

/// MDEC1 status: data-out FIFO empty.
pub const STATUS_OUT_EMPTY: u32 = 1 << 31;
/// MDEC1 status: data-in FIFO full.
pub const STATUS_IN_FULL: u32 = 1 << 30;
/// MDEC1 status: a command is receiving or processing parameters.
pub const STATUS_BUSY: u32 = 1 << 29;
/// MDEC1 status: data-in request (DMA0 enabled and the MDEC wants data).
pub const STATUS_IN_REQUEST: u32 = 1 << 28;
/// MDEC1 status: data-out request.
pub const STATUS_OUT_REQUEST: u32 = 1 << 27;

/// MDEC1 control: abort everything and reset. Tables survive it.
pub const CONTROL_RESET: u32 = 0x8000_0000;
/// MDEC1 control: enable the DMA0 and DMA1 requests.
pub const CONTROL_ENABLE_DMA: u32 = 0x6000_0000;
/// Command 2 for luma and chroma: 32 parameter words follow.
pub const COMMAND_SET_QUANT: u32 = 0x4000_0001;
/// Command 3: 32 parameter words follow.
pub const COMMAND_SET_SCALE: u32 = 0x6000_0000;

/// Spin budget for one status wait (reset settle, request, FIFO room).
/// The console settles a reset in tens of cycles; this is a few ms.
pub const SETTLE_SPINS: u32 = 20_000;
/// Status reads to spend after a reset looks settled. A reset from an idle
/// MDEC can read idle before it has finished, so waiting for busy to drop
/// is not enough on its own. Each read is an MMIO access, several cycles,
/// so this is a few hundred cycles against the tens a reset takes.
const RESET_TAIL_READS: u32 = 64;
/// Enable writes to try before giving up on the data-in request.
const ENABLE_ATTEMPTS: u8 = 4;

/// Raw MDEC1 status word.
#[inline(always)]
pub fn status() -> u32 {
    // SAFETY: MDEC status register read.
    unsafe { psx_io::read32(MDEC1) }
}

/// Wait until `status() & mask == want`. `false` on timeout.
fn wait_status(mask: u32, want: u32, spins: u32) -> bool {
    let mut n = 0;
    while status() & mask != want {
        if n >= spins {
            return false;
        }
        n += 1;
    }
    true
}

/// Reset the MDEC, wait for the reset to finish, then enable its DMA
/// requests on both channels. `false` if the MDEC stayed busy after the
/// reset (the enable is still written).
pub fn reset() -> bool {
    dma::abort(Channel::MdecIn);
    dma::abort(Channel::MdecOut);
    dma::enable_channel(Channel::MdecIn);
    dma::enable_channel(Channel::MdecOut);
    // SAFETY: MDEC control register write.
    unsafe { psx_io::write32(MDEC1, CONTROL_RESET) };
    let settled = wait_status(STATUS_BUSY, 0, SETTLE_SPINS);
    let mut n = 0;
    while n < RESET_TAIL_READS {
        let _ = status();
        n += 1;
    }
    // SAFETY: MDEC control register write.
    unsafe { psx_io::write32(MDEC1, CONTROL_ENABLE_DMA) };
    settled
}

/// Send `words` (a multiple of 32) to the MDEC over DMA0 without waiting.
///
/// # Safety
/// `words` must stay alive and unmodified until DMA0 completes.
unsafe fn dma_in(words: *const u32, count: usize) {
    dma::abort(Channel::MdecIn);
    dma::set_madr(Channel::MdecIn, words as u32);
    dma::set_bcr_block(
        Channel::MdecIn,
        DMA_BLOCK_WORDS as u16,
        (count / DMA_BLOCK_WORDS) as u16,
    );
    dma::set_chcr(Channel::MdecIn, CHCR_IN);
}

/// How [`load_tables`] got the tables in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tables {
    /// Enable writes, summed over both commands, before the MDEC raised its
    /// data-in request: 2 when every first write held. 0 means it never
    /// asked for data, so DMA0 decodes will not run either.
    pub enable_writes: u8,
    /// Tables that went in over the CPU because the MDEC never asked for
    /// DMA0 data or DMA0 wedged part way (0, 1 or 2).
    pub cpu_uploads: u8,
}

impl Tables {
    /// The MDEC asked for data on both commands, so DMA0 feeds it.
    pub fn dma_ready(&self) -> bool {
        self.enable_writes != 0 && self.cpu_uploads == 0
    }
}

/// Write `command`, then feed its 32 parameter words: over DMA0 when the
/// MDEC asks for data, else over the CPU. Returns (enable writes before the
/// request rose, 0 if never; went over the CPU), or `None` if the MDEC
/// would not take the words at all.
fn upload(command: u32, words: &[u32; 32]) -> Option<(u8, bool)> {
    if !wait_status(STATUS_BUSY, 0, SETTLE_SPINS) {
        return None;
    }
    // SAFETY: MDEC command write.
    unsafe { psx_io::write32(MDEC0, command) };
    let mut writes = 0u8;
    let mut asked = wait_status(STATUS_IN_REQUEST, STATUS_IN_REQUEST, SETTLE_SPINS);
    while !asked && writes + 1 < ENABLE_ATTEMPTS {
        // The enable can be lost to a reset that had not finished.
        // SAFETY: MDEC control register write, no reset bit.
        unsafe { psx_io::write32(MDEC1, CONTROL_ENABLE_DMA) };
        writes += 1;
        asked = wait_status(STATUS_IN_REQUEST, STATUS_IN_REQUEST, SETTLE_SPINS);
    }
    let enable_writes = if asked { writes + 1 } else { 0 };
    if asked {
        // SAFETY: `words` is a static table; the transfer is waited out.
        unsafe { dma_in(words.as_ptr(), 32) };
        if dma::wait_done(Channel::MdecIn, DMA_SPINS) {
            return Some((enable_writes, false));
        }
        // Part of the table went in: start the command over.
        dma::abort(Channel::MdecIn);
        reset();
        if !wait_status(STATUS_BUSY, 0, SETTLE_SPINS) {
            return None;
        }
        // SAFETY: MDEC command write.
        unsafe { psx_io::write32(MDEC0, command) };
    }
    for &word in words {
        if !wait_status(STATUS_IN_FULL, 0, SETTLE_SPINS) {
            return None;
        }
        // SAFETY: MDEC parameter write.
        unsafe { psx_io::write32(MDEC0, word) };
    }
    Some((enable_writes, true))
}

/// Upload the standard quantization tables and the IDCT basis. `None` if
/// the MDEC would take them neither over DMA0 nor over the CPU.
pub fn load_tables() -> Option<Tables> {
    let (quant_writes, quant_cpu) = upload(COMMAND_SET_QUANT, &QUANT_WORDS)?;
    let (scale_writes, scale_cpu) = upload(COMMAND_SET_SCALE, &SCALE_WORDS)?;
    if !wait_status(STATUS_BUSY, 0, SETTLE_SPINS) {
        return None;
    }
    Some(Tables {
        enable_writes: if quant_writes == 0 || scale_writes == 0 {
            0
        } else {
            quant_writes + scale_writes
        },
        cpu_uploads: quant_cpu as u8 + scale_cpu as u8,
    })
}

/// Upload both tables over CPU writes to MDEC0 only, after [`reset`]. No
/// DMA involved: the control path for a console whose DMA0 will not feed
/// the MDEC. `false` if the input FIFO never made room.
pub fn load_tables_cpu() -> bool {
    for (command, words) in [
        (COMMAND_SET_QUANT, &QUANT_WORDS),
        (COMMAND_SET_SCALE, &SCALE_WORDS),
    ] {
        if !wait_status(STATUS_BUSY, 0, SETTLE_SPINS) {
            return false;
        }
        write_command(command);
        for &word in words {
            if !wait_status(STATUS_IN_FULL, 0, SETTLE_SPINS) {
                return false;
            }
            write_command(word);
        }
    }
    wait_status(STATUS_BUSY, 0, SETTLE_SPINS)
}

/// Write one word to MDEC0: a command, or a parameter the CPU feeds itself.
#[inline(always)]
pub fn write_command(word: u32) {
    // SAFETY: MDEC command/parameter write.
    unsafe { psx_io::write32(MDEC0, word) }
}

/// Read one word of decoded output from MDEC0 (the CPU path; DMA1 is the
/// usual one). Garbage when the output FIFO is empty.
#[inline(always)]
pub fn read_data() -> u32 {
    // SAFETY: MDEC data read.
    unsafe { psx_io::read32(MDEC0) }
}

/// Start decoding `words` 32-bit words of run-length data (a multiple of 32,
/// as [`crate::bs::decode_frame`] returns). `mode` is [`DECODE_15BPP`] or
/// [`DECODE_24BPP`], optionally with [`DECODE_STP`].
///
/// # Safety
/// `rle` must stay alive and unmodified until [`decode_finish`] returns.
pub unsafe fn decode_start(rle: &[u32], words: usize, mode: u32) {
    // SAFETY: MDEC command write, then a DMA0 kick the caller keeps alive.
    unsafe {
        psx_io::write32(MDEC0, mode | (words as u32 & 0xFFFF));
        dma_in(rle.as_ptr(), words);
    }
}

/// Pull the next `dst.len()` words (a multiple of 32) of decoded pixels
/// over DMA1 and wait for them. For 15bpp one 16-pixel-wide column of
/// height `h` is `8 * h` words. `false` if DMA1 wedged.
pub fn read_column(dst: &mut [u32]) -> bool {
    dma::abort(Channel::MdecOut);
    dma::set_madr(Channel::MdecOut, dst.as_mut_ptr() as u32);
    dma::set_bcr_block(
        Channel::MdecOut,
        DMA_BLOCK_WORDS as u16,
        (dst.len() / DMA_BLOCK_WORDS) as u16,
    );
    dma::set_chcr(Channel::MdecOut, CHCR_OUT);
    if dma::wait_done(Channel::MdecOut, DMA_SPINS) {
        true
    } else {
        dma::abort(Channel::MdecOut);
        false
    }
}

/// Confirm DMA0 finished feeding the frame. `false` (after aborting the
/// channel) if it is still busy, e.g. the frame held more data than was
/// read back.
pub fn decode_finish() -> bool {
    if dma::wait_done(Channel::MdecIn, DMA_SPINS) {
        true
    } else {
        dma::abort(Channel::MdecIn);
        false
    }
}
