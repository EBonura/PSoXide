//! Stream the capture payload out of the SPU as audio.
//!
//! A capture card records audio as well as video, and audio is a far wider
//! pipe than a photographed QR symbol: the whole binary goes out continuously
//! and hands-free instead of being paged through three or four still frames.
//! The SPU loops the sample in hardware, so the payload repeats forever with
//! no CPU involvement and a decode spoiled by a glitch is recovered from the
//! next repetition rather than another burn.
//!
//! Modulation is binary FSK carried by square waves. FSK is chosen because it
//! is amplitude-independent: capture chains apply AGC and arbitrary volume,
//! which would wreck any on/off keying, but cannot change which tone is
//! present.
//!
//! Each bit is exactly one ADPCM block (28 samples at 44.1 kHz, 0.635 ms), so
//! bit boundaries land on block boundaries and the two tones are chosen to fit
//! a whole number of cycles into one block. That makes blocks concatenate
//! without a phase discontinuity, which would otherwise smear the spectrum and
//! blunt the host's discrimination.
//!
//! Wire format, MSB first within each byte:
//!   64 bits alternating 1/0   preamble, for the host to lock bit timing
//!   16 bits SYNC_WORD         frame start
//!   16 bits payload length    little-endian byte count
//!   N  bytes payload
//!   32 bits CRC-32            over the payload bytes only

use psx_spu as spu;

use crate::photo::crc32;

/// Samples in one ADPCM block, and therefore in one transmitted bit.
const SAMPLES_PER_BLOCK: usize = 28;
/// Bytes in one ADPCM block: 1 shift/filter, 1 flags, 14 data.
const BLOCK_BYTES: usize = 16;
/// Distinctive frame marker. Not all-ones or all-zeros, so it cannot be
/// confused with the preamble or with silence.
const SYNC_WORD: u16 = 0x1ACF;
/// First usable SPU RAM address; the low 4 KiB is reserved by the hardware.
const SPU_BASE: u32 = 0x1010;
/// Blocks staged in main RAM before each upload. Keeps the staging buffer at
/// 4 KiB instead of materialising the whole ~290 KiB stream twice.
const STAGE_BLOCKS: usize = 256;

/// ADPCM shift for the tone nibbles. Bigger amplitude gives the host more
/// signal-to-noise; this stops short of full scale so a capture chain with a
/// little gain does not clip and distort the tone into its neighbour.
const TONE_SHIFT: u8 = 1;
/// Nibbles of a square wave, one full period per `PERIOD` samples.
const HIGH_NIBBLE: u8 = 0x7;
const LOW_NIBBLE: u8 = 0x8; // -8 in 4-bit two's complement

/// Bit 0 tone: 2 cycles per block -> 44100 / 14 = 3150 Hz.
const PERIOD_ZERO: usize = 14;
/// Bit 1 tone: 1 cycle per block -> 44100 / 28 = 1575 Hz.
const PERIOD_ONE: usize = 28;
/// Blocks of each pure tone prepended as a calibration section.
///
/// If a decode fails there is otherwise no way to tell whether the chain
/// mangled the amplitude, the frequency response, or the timing. A known tone
/// at each frequency, ahead of any data, can be measured offline and the
/// decoder adapted without burning another disc.
const CALIBRATION_BLOCKS: usize = 64;

/// Playback rates, as SPU pitch values. The uploaded stream is IDENTICAL for
/// every rate: dividing the pitch stretches each block over proportionally
/// more output samples, which lowers both tones and the baud rate together and
/// multiplies the energy per bit. A slower mode therefore costs no extra SPU
/// RAM, which matters because the fast stream already fills most of it.
pub(crate) const RATE_DIVISORS: [u16; 4] = [
    0x1000, // 1575 baud, tones 1575/3150 Hz
    0x0800, // 787 baud, tones 787/1575 Hz
    0x0400, // 394 baud, tones 394/787 Hz
    0x0200, // 197 baud, tones 197/394 Hz
];

/// Build the 16-byte ADPCM block for one bit value.
const fn tone_block(period: usize, flags: u8) -> [u8; BLOCK_BYTES] {
    let mut block = [0u8; BLOCK_BYTES];
    block[0] = TONE_SHIFT;
    block[1] = flags;
    // 28 nibbles, two per byte, low nibble first.
    let mut sample = 0usize;
    while sample < SAMPLES_PER_BLOCK {
        let nibble = if (sample % period) * 2 < period {
            HIGH_NIBBLE
        } else {
            LOW_NIBBLE
        };
        let byte = 2 + sample / 2;
        if sample % 2 == 0 {
            block[byte] |= nibble;
        } else {
            block[byte] |= nibble << 4;
        }
        sample += 1;
    }
    block
}

/// Upload `payload` to SPU RAM and start transmitting at the fastest rate.
///
/// Returns the number of bits transmitted, or 0 if the stream would not fit in
/// SPU RAM. Runs once, after the battery: it takes over voice 0 and ~290 KiB
/// of SPU RAM, so any probe that owns sample memory must run before it.
pub(crate) fn prepare(payload: &[u8]) -> u32 {
    let total_bits = CALIBRATION_BLOCKS * 2 + 64 + 16 + 16 + payload.len() * 8 + 32;
    let bytes_needed = total_bits * BLOCK_BYTES;
    if SPU_BASE as usize + bytes_needed > 512 * 1024 {
        return 0;
    }

    let crc = crc32(payload);
    let mut stage = [0u8; STAGE_BLOCKS * BLOCK_BYTES];
    let mut staged = 0usize;
    let mut addr = SPU_BASE;
    let mut emitted = 0usize;

    // Closure-free loop: emit every bit of the frame in order, flushing the
    // staging buffer to SPU RAM whenever it fills.
    let mut index = 0usize;
    while index < total_bits {
        let bit = frame_bit(index, payload, crc, total_bits);
        let first = index == 0;
        let last = index + 1 == total_bits;
        let flags = if first {
            0x04 // loop start
        } else if last {
            0x03 // end of sample + repeat from the loop address
        } else {
            0x00
        };
        let block = if bit {
            tone_block(PERIOD_ONE, flags)
        } else {
            tone_block(PERIOD_ZERO, flags)
        };
        let base = staged * BLOCK_BYTES;
        let mut byte = 0;
        while byte < BLOCK_BYTES {
            stage[base + byte] = block[byte];
            byte += 1;
        }
        staged += 1;
        emitted += 1;
        if staged == STAGE_BLOCKS {
            spu::upload_adpcm(spu::SpuAddr::new(addr), &stage[..staged * BLOCK_BYTES]);
            addr += (staged * BLOCK_BYTES) as u32;
            staged = 0;
        }
        index += 1;
    }
    if staged != 0 {
        spu::upload_adpcm(spu::SpuAddr::new(addr), &stage[..staged * BLOCK_BYTES]);
    }

    // Verify the stream actually survived in SPU RAM before playing it. A
    // console capture stopped 13% in, which the emulator cannot reproduce, so
    // the disc has to be able to say whether the upload SURVIVED rather than
    // leaving that to be inferred from a failed decode.
    let verified = verify_upload(addr_end(total_bits));

    // On by default. The readout is the whole reason the disc can hand a
    // payload back without photographing five symbols, and leaving it off cost
    // a console session: the operator has no reason to know a silent disc is
    // withholding anything. The earlier problem was VOLUME, not existence, and
    // that is fixed in set_rate.
    set_rate(0);
    // Diagnostic, NOT a gate. SPU DMA readback is documented in this repo as
    // unreliable as an oracle (its unstable read mode corrupts FIFO
    // boundaries), and gating on it made the emulator report a failed upload
    // for a stream that was demonstrably fine. The flag rides in the high bit
    // so the screen can show a suspected truncation without suppressing a
    // transmission that may well be good.
    if verified {
        emitted as u32
    } else {
        emitted as u32 | 0x8000_0000
    }
}

/// Byte address one past the uploaded stream.
const fn addr_end(total_bits: usize) -> u32 {
    SPU_BASE + (total_bits * BLOCK_BYTES) as u32
}

/// Read back the last block of the uploaded stream and check it is the block
/// that was written.
///
/// The final block is the one carrying the end/repeat flags, and it sits at the
/// far end of the stream: if anything reclaims SPU RAM partway through, this is
/// what goes missing. Checking it distinguishes "upload truncated or clobbered"
/// from "modulation wrong", which a silent recording alone cannot.
fn verify_upload(end: u32) -> bool {
    let expected = tone_block(PERIOD_ZERO, 0x03);
    let mut got = [0u32; 4];
    crate::spu_dma_read(end - BLOCK_BYTES as u32, &mut got);
    let mut byte = 0usize;
    while byte < BLOCK_BYTES {
        let word = got[byte / 4];
        let lane = (word >> ((byte % 4) * 8)) as u8;
        // Flags and shift must match; the tone nibbles are checked too, since
        // reverb scribble would disturb them as readily.
        if lane != expected[byte] {
            return false;
        }
        byte += 1;
    }
    true
}

/// Value of frame bit `index`, MSB first within each byte.
fn frame_bit(index: usize, payload: &[u8], crc: u32, total_bits: usize) -> bool {
    // Calibration: a solid run of each tone before any data.
    if index < CALIBRATION_BLOCKS {
        return false;
    }
    if index < CALIBRATION_BLOCKS * 2 {
        return true;
    }
    let index = index - CALIBRATION_BLOCKS * 2;
    if index < 64 {
        // Alternating preamble: a clean 1/0 sequence the host can lock to.
        return index % 2 == 0;
    }
    let index = index - 64;
    if index < 16 {
        return (SYNC_WORD >> (15 - index)) & 1 == 1;
    }
    let index = index - 16;
    if index < 16 {
        let length = payload.len() as u16;
        return (length >> (15 - index)) & 1 == 1;
    }
    let index = index - 16;
    let payload_bits = payload.len() * 8;
    if index < payload_bits {
        let byte = payload[index / 8];
        return (byte >> (7 - (index % 8))) & 1 == 1;
    }
    let index = index - payload_bits;
    debug_assert!(index < 32 && total_bits > 0);
    (crc >> (31 - index)) & 1 == 1
}

/// Play the uploaded stream on voice 0, looping in hardware.
/// Silence the readout.
pub(crate) fn stop() {
    spu::Voice::key_off(spu::Voice::V0.mask());
    spu::Voice::V0.set_volume(spu::Volume::SILENCE, spu::Volume::SILENCE);
}

/// Key the transmission at `RATE_DIVISORS[index]`.
///
/// Operator-selectable so a chain that cannot decode the fast mode can be
/// dropped to a slower one on the spot, instead of needing another burn.
pub(crate) fn set_rate(index: usize) {
    let voice = spu::Voice::V0;
    spu::Voice::key_off(voice.mask());
    voice.set_start_addr(spu::SpuAddr::new(SPU_BASE));
    voice.set_loop_addr(spu::SpuAddr::new(SPU_BASE));
    let divisor = RATE_DIVISORS[index % RATE_DIVISORS.len()];
    // 0x1000 plays at the native 44.1 kHz, so one block is exactly 28 output
    // samples. Lower values stretch every block proportionally.
    voice.set_pitch(spu::Pitch::raw(divisor));
    // A quarter scale, not full. These are square waves: at MAX they came out
    // of the TV as a harsh screech loud enough to be alarming, and the decoder
    // recovers the payload through a 20x gain range anyway, so the volume was
    // buying nothing.
    let level = spu::Volume::linear(1, 4);
    voice.set_volume(level, level);
    // Adsr::sample(): instant attack, sustain level MAX, so the level holds
    // flat for as long as the voice plays.
    //
    // NOT passthrough(). That is all-zeroes, which means sustain level 0, and
    // on real hardware the envelope therefore decays to silence shortly after
    // key-on: a console recording carried only ~3 seconds of a 13.6 second
    // payload before fading out. PSoXide does not model that decay and looped
    // happily, so the emulator could not have caught this.
    voice.set_adsr(spu::Adsr::sample());
    spu::set_main_volume(spu::Volume::HALF, spu::Volume::HALF);
    spu::Voice::key_on(voice.mask());
}
