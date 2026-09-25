// SPDX-License-Identifier: GPL-2.0-or-later
//! MDEC driver: table upload, decode command, DMA0 in, DMA1 out.
//!
//! Decode protocol, as commercial players drive it:
//!
//! 1. [`reset`] once, then [`load_tables`] (quantization + IDCT tables).
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
static QUANT_WORDS: [u32; 32] = build_quant();

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

static SCALE_WORDS: [u32; 32] = build_scale();

const fn build_scale() -> [u32; 32] {
    let mut words = [0u32; 32];
    let mut w = 0;
    while w < 32 {
        words[w] = (SCALE[2 * w] as u16 as u32) | ((SCALE[2 * w + 1] as u16 as u32) << 16);
        w += 1;
    }
    words
}

/// Reset the MDEC and enable its DMA requests on both channels.
pub fn reset() {
    // SAFETY: MDEC control register writes.
    unsafe {
        psx_io::write32(MDEC1, 0x8000_0000);
        psx_io::write32(MDEC1, 0x6000_0000);
    }
    dma::enable_channel(Channel::MdecIn);
    dma::enable_channel(Channel::MdecOut);
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

/// Upload the standard quantization tables and the IDCT basis.
/// `false` if a DMA0 transfer wedged.
pub fn load_tables() -> bool {
    // SAFETY: both tables are statics; each transfer is waited out.
    unsafe {
        psx_io::write32(MDEC0, 0x4000_0001);
        dma_in(QUANT_WORDS.as_ptr(), 32);
        if !dma::wait_done(Channel::MdecIn, DMA_SPINS) {
            dma::abort(Channel::MdecIn);
            return false;
        }
        psx_io::write32(MDEC0, 0x6000_0000);
        dma_in(SCALE_WORDS.as_ptr(), 32);
        if !dma::wait_done(Channel::MdecIn, DMA_SPINS) {
            dma::abort(Channel::MdecIn);
            return false;
        }
    }
    true
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
