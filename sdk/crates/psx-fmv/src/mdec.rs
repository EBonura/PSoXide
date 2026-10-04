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

use psx_hw::mdec::{self as hw, MDEC0, MDEC1};
use psx_io::dma::{self, Channel};

use crate::rle::{self, RleLengthError};

/// DMA block size the MDEC channels use, in words.
pub const DMA_BLOCK_WORDS: usize = crate::rle::BLOCK_WORDS;

/// Spin budget for one table upload or column transfer.
pub const DMA_SPINS: u32 = 400_000;

// CHCR: to device / from device, block sync, start.
const CHCR_IN: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START;
const CHCR_OUT: u32 = psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START;

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

/// The IDCT matrix for MDEC command 3, generated from the DCT basis in
/// [`crate::idct`].
pub static SCALE_WORDS: [u32; 32] = crate::idct::MATRIX_WORDS;

// Register constants moved to psx-hw; these forward until games repin.

/// Moved to [`psx_hw::mdec::DECODE_15BPP`].
#[deprecated(note = "moved to `psx_hw::mdec::DECODE_15BPP`")]
pub const DECODE_15BPP: u32 = psx_hw::mdec::DECODE_15BPP;
/// Moved to [`psx_hw::mdec::DECODE_24BPP`].
#[deprecated(note = "moved to `psx_hw::mdec::DECODE_24BPP`")]
pub const DECODE_24BPP: u32 = psx_hw::mdec::DECODE_24BPP;
/// Moved to [`psx_hw::mdec::DECODE_STP`].
#[deprecated(note = "moved to `psx_hw::mdec::DECODE_STP`")]
pub const DECODE_STP: u32 = psx_hw::mdec::DECODE_STP;
/// Moved to [`psx_hw::mdec::STATUS_OUT_EMPTY`].
#[deprecated(note = "moved to `psx_hw::mdec::STATUS_OUT_EMPTY`")]
pub const STATUS_OUT_EMPTY: u32 = psx_hw::mdec::STATUS_OUT_EMPTY;
/// Moved to [`psx_hw::mdec::STATUS_IN_FULL`].
#[deprecated(note = "moved to `psx_hw::mdec::STATUS_IN_FULL`")]
pub const STATUS_IN_FULL: u32 = psx_hw::mdec::STATUS_IN_FULL;
/// Moved to [`psx_hw::mdec::STATUS_BUSY`].
#[deprecated(note = "moved to `psx_hw::mdec::STATUS_BUSY`")]
pub const STATUS_BUSY: u32 = psx_hw::mdec::STATUS_BUSY;
/// Moved to [`psx_hw::mdec::STATUS_IN_REQUEST`].
#[deprecated(note = "moved to `psx_hw::mdec::STATUS_IN_REQUEST`")]
pub const STATUS_IN_REQUEST: u32 = psx_hw::mdec::STATUS_IN_REQUEST;
/// Moved to [`psx_hw::mdec::STATUS_OUT_REQUEST`].
#[deprecated(note = "moved to `psx_hw::mdec::STATUS_OUT_REQUEST`")]
pub const STATUS_OUT_REQUEST: u32 = psx_hw::mdec::STATUS_OUT_REQUEST;
/// Moved to [`psx_hw::mdec::CONTROL_RESET`].
#[deprecated(note = "moved to `psx_hw::mdec::CONTROL_RESET`")]
pub const CONTROL_RESET: u32 = psx_hw::mdec::CONTROL_RESET;
/// Moved to [`psx_hw::mdec::CONTROL_ENABLE_DMA`].
#[deprecated(note = "moved to `psx_hw::mdec::CONTROL_ENABLE_DMA`")]
pub const CONTROL_ENABLE_DMA: u32 = psx_hw::mdec::CONTROL_ENABLE_DMA;
/// Moved to [`psx_hw::mdec::COMMAND_SET_QUANT`].
#[deprecated(note = "moved to `psx_hw::mdec::COMMAND_SET_QUANT`")]
pub const COMMAND_SET_QUANT: u32 = psx_hw::mdec::COMMAND_SET_QUANT;
/// Moved to [`psx_hw::mdec::COMMAND_SET_SCALE`].
#[deprecated(note = "moved to `psx_hw::mdec::COMMAND_SET_SCALE`")]
pub const COMMAND_SET_SCALE: u32 = psx_hw::mdec::COMMAND_SET_SCALE;

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
    unsafe { psx_io::read_u32(MDEC1) }
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
    unsafe { psx_io::write_u32(MDEC1, hw::CONTROL_RESET) };
    let settled = wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS);
    let mut n = 0;
    while n < RESET_TAIL_READS {
        let _ = status();
        n += 1;
    }
    // SAFETY: MDEC control register write.
    unsafe { psx_io::write_u32(MDEC1, hw::CONTROL_ENABLE_DMA) };
    settled
}

/// Send `blocks` 32-word blocks from `words` to the MDEC over DMA0 without
/// waiting.
///
/// # Safety
/// `blocks` must not be 0 (the controller reads 0 as 65,536 blocks), and
/// `blocks * 32` words at `words` must stay alive and unmodified until DMA0
/// completes.
unsafe fn dma_in(words: *const u32, blocks: u16) {
    debug_assert!(blocks != 0, "a zero block count is 65,536 blocks");
    dma::abort(Channel::MdecIn);
    // SAFETY: the channel was just aborted, so it is idle; the caller keeps
    // the `blocks * 32` words at `words` alive and unmodified until DMA0
    // completes, and `blocks` is not 0.
    unsafe {
        dma::start(
            Channel::MdecIn,
            dma::Transfer {
                address: words as u32,
                size: dma::size_blocks(DMA_BLOCK_WORDS as u16, blocks),
                control: CHCR_IN,
            },
        )
    };
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
    pub fn is_dma_ready(&self) -> bool {
        self.enable_writes != 0 && self.cpu_uploads == 0
    }

    /// Renamed to [`Tables::is_dma_ready`].
    #[deprecated(note = "renamed to `is_dma_ready`")]
    #[inline(always)]
    pub fn dma_ready(&self) -> bool {
        self.is_dma_ready()
    }
}

/// Write `command`, then feed its 32 parameter words: over DMA0 when the
/// MDEC asks for data, else over the CPU. Returns (enable writes before the
/// request rose, 0 if never; went over the CPU), or `None` if the MDEC
/// would not take the words at all.
fn upload(command: u32, words: &[u32; 32]) -> Option<(u8, bool)> {
    if !wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS) {
        return None;
    }
    // SAFETY: MDEC command write.
    unsafe { psx_io::write_u32(MDEC0, command) };
    let mut writes = 0u8;
    let mut asked = wait_status(hw::STATUS_IN_REQUEST, hw::STATUS_IN_REQUEST, SETTLE_SPINS);
    while !asked && writes + 1 < ENABLE_ATTEMPTS {
        // The enable can be lost to a reset that had not finished.
        // SAFETY: MDEC control register write, no reset bit.
        unsafe { psx_io::write_u32(MDEC1, hw::CONTROL_ENABLE_DMA) };
        writes += 1;
        asked = wait_status(hw::STATUS_IN_REQUEST, hw::STATUS_IN_REQUEST, SETTLE_SPINS);
    }
    let enable_writes = if asked { writes + 1 } else { 0 };
    if asked {
        // SAFETY: `words` is a static table; the transfer is waited out.
        unsafe { dma_in(words.as_ptr(), 1) };
        if dma::wait_done(Channel::MdecIn, DMA_SPINS) {
            return Some((enable_writes, false));
        }
        // Part of the table went in: start the command over.
        dma::abort(Channel::MdecIn);
        reset();
        if !wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS) {
            return None;
        }
        // SAFETY: MDEC command write.
        unsafe { psx_io::write_u32(MDEC0, command) };
    }
    for &word in words {
        if !wait_status(hw::STATUS_IN_FULL, 0, SETTLE_SPINS) {
            return None;
        }
        // SAFETY: MDEC parameter write.
        unsafe { psx_io::write_u32(MDEC0, word) };
    }
    Some((enable_writes, true))
}

/// Upload the standard quantization tables and the IDCT basis. `None` if
/// the MDEC would take them neither over DMA0 nor over the CPU.
pub fn load_tables() -> Option<Tables> {
    let (quant_writes, quant_cpu) = upload(hw::COMMAND_SET_QUANT, &QUANT_WORDS)?;
    let (scale_writes, scale_cpu) = upload(hw::COMMAND_SET_SCALE, &SCALE_WORDS)?;
    if !wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS) {
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
        (hw::COMMAND_SET_QUANT, &QUANT_WORDS),
        (hw::COMMAND_SET_SCALE, &SCALE_WORDS),
    ] {
        if !wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS) {
            return false;
        }
        write_command(command);
        for &word in words {
            if !wait_status(hw::STATUS_IN_FULL, 0, SETTLE_SPINS) {
                return false;
            }
            write_command(word);
        }
    }
    wait_status(hw::STATUS_BUSY, 0, SETTLE_SPINS)
}

/// Write one word to MDEC0: a command, or a parameter the CPU feeds itself.
#[inline(always)]
pub fn write_command(word: u32) {
    // SAFETY: MDEC command/parameter write.
    unsafe { psx_io::write_u32(MDEC0, word) }
}

/// Read one word of decoded output from MDEC0 (the CPU path; DMA1 is the
/// usual one). Garbage when the output FIFO is empty.
#[inline(always)]
pub fn read_data() -> u32 {
    // SAFETY: MDEC data read.
    unsafe { psx_io::read_u32(MDEC0) }
}

/// Start decoding `words` 32-bit words of run-length data (a multiple of 32,
/// as [`crate::bitstream::decode_frame`] returns). `mode` is
/// [`psx_hw::mdec::DECODE_15BPP`] or [`psx_hw::mdec::DECODE_24BPP`],
/// optionally with [`psx_hw::mdec::DECODE_STP`].
///
/// [`decode`] is the safe form: it holds the borrow of `rle` until DMA0 is
/// done with it.
///
/// # Errors
/// [`RleLengthError`] when `words` is 0, not a multiple of 32, longer than
/// the decode command can announce ([`crate::rle::MAX_WORDS`]) or longer
/// than `rle`. Nothing is written to the MDEC then and no DMA starts.
///
/// # Safety
/// On `Ok`, DMA0 keeps reading `rle` after this returns. The caller must
/// keep `rle` alive and unmodified until [`decode_finish`] returns, and must
/// call it before the storage is reused or freed.
pub unsafe fn decode_start(rle: &[u32], words: usize, mode: u32) -> Result<(), RleLengthError> {
    if words > rle.len() {
        return Err(RleLengthError::PastBuffer);
    }
    let blocks = rle::dma_block_count(words)?;
    // SAFETY: MMIO write to MDEC0, then a DMA0 kick of `blocks` (not 0)
    // 32-word blocks, exactly the first `words` words of `rle` (checked
    // above), which the caller keeps alive until `decode_finish`.
    unsafe {
        psx_io::write_u32(MDEC0, mode | words as u32);
        dma_in(rle.as_ptr(), blocks);
    }
    Ok(())
}

/// Decode all of `rle` (a multiple of 32 words, as [`crate::bitstream::decode_frame`]
/// returns) with DMA0 feeding the MDEC, the safe form of [`decode_start`].
///
/// `columns` runs while DMA0 feeds the MDEC and pulls the output, normally
/// with [`read_column`] once per column. Returns its result and
/// [`decode_finish`]'s: `false` there means DMA0 was still busy and has been
/// aborted.
///
/// # Errors
/// [`RleLengthError`] when `rle` is empty, not a whole number of 32-word
/// blocks or longer than [`crate::rle::MAX_WORDS`]; `columns` does not run.
pub fn decode<R>(
    rle: &[u32],
    mode: u32,
    columns: impl FnOnce() -> R,
) -> Result<(R, bool), RleLengthError> {
    // Finishes the decode on every exit, an unwinding `columns` included.
    struct Finish(bool);
    impl Drop for Finish {
        fn drop(&mut self) {
            if !self.0 {
                decode_finish();
            }
        }
    }
    // SAFETY: `rle` stays borrowed until this function returns. On `Err`
    // no DMA started; on `Ok` every return path runs decode_finish first
    // (directly, or via `Finish` when unwinding), which returns only once
    // DMA0 is done or aborted.
    unsafe { decode_start(rle, rle.len(), mode)? };
    let mut finish = Finish(false);
    let out = columns();
    finish.0 = true;
    Ok((out, decode_finish()))
}

/// Pull the next `dst.len()` words (a multiple of 32) of decoded pixels
/// over DMA1 and wait for them. For 15bpp one 16-pixel-wide column of
/// height `h` is `8 * h` words. `false` if DMA1 wedged.
pub fn read_column(dst: &mut [u32]) -> bool {
    let blocks = dst.len() / DMA_BLOCK_WORDS;
    // Silicon reads a zero block count as 65,536 blocks, so a slice shorter
    // than one block would be overrun by megabytes; one too long for BCR
    // would be cut short. Refuse both rather than write past `dst`.
    let Ok(blocks) = u16::try_from(blocks) else {
        return false;
    };
    if blocks == 0 {
        return false;
    }
    dma::abort(Channel::MdecOut);
    // SAFETY: the channel was just aborted, so it is idle. The transfer
    // writes `blocks * DMA_BLOCK_WORDS` words, no more than `dst.len()`,
    // into `dst`, borrowed exclusively until this function returns; the
    // wait below, or the abort on a wedge, ends it before then.
    unsafe {
        dma::start(
            Channel::MdecOut,
            dma::Transfer {
                address: dst.as_mut_ptr() as u32,
                size: dma::size_blocks(DMA_BLOCK_WORDS as u16, blocks),
                control: CHCR_OUT,
            },
        )
    };
    dma::wait_or_abort(Channel::MdecOut, DMA_SPINS)
}

/// Confirm DMA0 finished feeding the frame. `false` (after aborting the
/// channel) if it is still busy, e.g. the frame held more data than was
/// read back.
pub fn decode_finish() -> bool {
    dma::wait_or_abort(Channel::MdecIn, DMA_SPINS)
}
