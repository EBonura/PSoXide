//! SB4: the capture-ring readback.
//!
//! The SPU writes, every 44.1 kHz tick, one signed sample of voice 1's and
//! voice 3's post-envelope output into two 512-sample rings at SPU RAM
//! 0x800 and 0xC00. Nothing on this disc has ever read them, and they are
//! the instrument the sound-parity effort is missing: a DMA readback of a
//! ring is the voice's decoded output IN NUMBERS, so ADPCM decode, gaussian
//! interpolation, ADSR stepping and the noise LFSR all become bit-exact
//! comparisons between silicon and emulator, with no capture card in the
//! chain. It is the same trick the GPU side already plays with VRAM
//! readback hashes, which is why the graphics are ahead of the sound.
//!
//! The ring's write index free-runs from power-on and is not readable, so a
//! naive snapshot lands at an arbitrary phase and hashes differently every
//! run. SPUSTAT bit 11 says which HALF of the rings is being written; the
//! probe waits for that flag to flip, keys the voice on the edge, waits for
//! the flip back, and then reads the half the key-on landed in while the
//! writer is busy in the other. That pins sample 0 of every snapshot to the
//! key-on tick within a poll's jitter, and makes the window's leading
//! silence a measurement in its own right: it is the key-on-to-first-output
//! latency, in ticks, which nothing else on this disc can see.
//!
//! Five segments, each keyed on a fresh edge:
//!
//!   SQUARE   voice 1, unity pitch: raw decode and mixing fidelity
//!   IMPULSE  voice 1 at pitch 0x0800: every source sample is consumed at
//!            two output phases, so an impulse train reads the gaussian
//!            interpolation kernel out sample by sample
//!   ENVRAMP  slow linear attack across the whole window: per-tick
//!            envelope stepping, the arithmetic SB1 could only spot-check
//!   NOISE    voice 1 with NON set: the LFSR sequence itself
//!   VOICE3   voice 3, same square: the other ring, and the voice-to-ring
//!            mapping
//!
//! The QR carries, per segment, the snapshot's SPUSTAT and late envelope,
//! the first-nonzero index, a CRC-32 of the full 256-sample half, and the
//! first 32 raw samples. The full payload also goes to the TTY, so a
//! headless emulator run needs no QR scanning.
//!
//! The rings capture the voice BEFORE its volume registers apply. That was
//! the documented claim; the 2026-08-07 console capture settled it, because
//! VOICE3 runs at half volume against V1's quarter and their rings come back
//! bit-identical (hash BACBD7D9 both). A post-volume tap could not do that.
//! Key-on alignment came out of the same capture: every segment shows nine
//! zero samples before the first envelope step.

use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};

use crate::console_tests::record;
use crate::payload::crc32;
use crate::{spu_dma_read, TimingRecord};

// ---- Rings ---------------------------------------------------------------

/// Voice-1 and voice-3 output rings. 0x400 bytes each: 512 samples, 11.6 ms.
const RING_V1: u32 = 0x0800;
const RING_V3: u32 = 0x0C00;
/// One ring half: what SPUSTAT bit 11 points away from, and what we read.
const HALF_BYTES: usize = 0x200;
const HALF_SAMPLES: usize = HALF_BYTES / 2;
const HALF_WORDS: usize = HALF_BYTES / 4;
/// SPUSTAT bit 11: writing to the second half of the capture buffers.
const STAT_HALF: u16 = 0x0800;
/// Bounded half-flag waits. The flag flips every 5.8 ms; at MMIO read cost
/// this bound is comfortably past two flips, and a flag that never moves is
/// itself a finding, not a hang. Hangs cost burns; this suite does not hang.
const POLL_BOUND: u32 = 2_000_000;

// ---- Sources -------------------------------------------------------------

/// Upload area. NOT 0x1100: that sits inside the standard sample-bank
/// region (banks start at 0x1010), and the v1.16 console run captured a
/// stale leftover instrument instead of these tables, invalidating the
/// tone-segment hashes as decode references. 0x4000 is clear of the rings,
/// the banks, and the A6/A7 round-trip area at 0x3000/0x3400.
const SPU_SQUARE_ADDR: u32 = 0x4000;
const SPU_IMPULSE_ADDR: u32 = SPU_SQUARE_ADDR + TABLE_BYTES as u32;
const TABLE_BLOCKS: usize = 4;
const TABLE_BYTES: usize = TABLE_BLOCKS * 16;
/// ADPCM shift for both tables; nibble 0x7 decodes to +0x3800.
const TABLE_SHIFT: u8 = 1;
const UNITY_PITCH: u16 = 0x1000;
/// IMPULSE plays at half speed so every source step lands on two output
/// phases and the interpolation kernel is read out directly.
const IMPULSE_PITCH: u16 = 0x0800;
/// Linear attack, shift 7: ~112 envelope units per tick, ~293 ticks to full
/// scale, so the ramp spans the whole 256-sample window with room to spare.
/// Sustain level max, so nothing after the attack moves.
const RAMP_ADSR: Adsr = Adsr {
    lower: 0x1C0F,
    upper: 0x0000,
};
/// Instant attack, max sustain: the window sees the waveform, not an envelope.
const FLAT_ADSR: Adsr = Adsr {
    lower: 0x000F,
    upper: 0x0000,
};
/// Quarter volume. The rings document themselves as PRE-volume taps, so the
/// snapshot must NOT scale with this; if it does, that is a real result.
const TONE_VOLUME: Volume = Volume::linear(1, 4);

// ---- Segments ------------------------------------------------------------

const SEGMENTS: usize = 5;
// Frames of audible tone per segment, then the gap. The capture itself
// happens inside frame 0; the tail is an operator cue, not the instrument.

// ---- Results -------------------------------------------------------------

/// Raw samples kept per segment beside the full-half CRC. 32 is enough to
/// see the interpolation kernel, the attack's first steps, the LFSR's first
/// words, and the key-on latency, while the hash still covers all 256.
const RAW_SAMPLES: usize = 32;

/// rec sb4_hash: ring_half_crc_low, ring_half_crc_high, first_nonzero_sample (5 records, 0x490-0x494)
pub(crate) const SB4_HASH: u16 = 0x490;
/// rec sb4_state: spustat, envelope, raw_sample_16 (5 records, 0x49A-0x49E)
pub(crate) const SB4_STATE: u16 = 0x49A;

// Shift 13 clocks the LFSR fast enough that the 32 raw window words read
// out an actual sequence; at the old shift 8 / step 2 the LFSR stepped only
// every ~128 ticks and the window saw a constant on both platforms.
const NOISE_SHIFT: u8 = 13;
const NOISE_STEP: u8 = 2;

/// One captured half.
#[derive(Copy, Clone)]
struct Snapshot {
    /// SPUSTAT immediately after the read; bit 15 set means a half-flag
    /// wait timed out and the rest of the row describes nothing.
    stat: u16,
    envx: u16,
    /// Index of the first nonzero sample: the key-on latency in ticks.
    /// 0xFFFF means the whole half read back silent.
    first: u16,
    hash: u32,
    raw: [i16; RAW_SAMPLES],
}

impl Snapshot {
    const fn empty() -> Self {
        Self {
            stat: 0,
            envx: 0,
            first: 0,
            hash: 0,
            raw: [0; RAW_SAMPLES],
        }
    }
}

/// Run the five segments and return their records.
pub(crate) fn run() -> [TimingRecord; SEGMENTS * 2] {
    spu::init();
    spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
    spu::enable_cd_audio(false);
    upload_tables();
    tty::println("hardware-tests: sb4 begin capture-ring readback");
    let mut out = [record(SB4_HASH, 0, 0, 0); SEGMENTS * 2];
    for segment in 0..SEGMENTS {
        let snap = run_segment(segment);
        // Let the keyed voice release and the capture settle before the next
        // edge; two frames is far past the ring period.
        Voice::release(all_used_mask());
        Voice::set_noise_mask(0);
        interrupts::wait_vblank();
        interrupts::wait_vblank();
        out[segment] = record(
            SB4_HASH + segment as u16,
            snap.hash & 0xFFFF,
            snap.hash >> 16,
            snap.first as u32,
        );
        out[SEGMENTS + segment] = record(
            SB4_STATE + segment as u16,
            snap.stat as u32,
            snap.envx as u32,
            snap.raw[16] as u16 as u32,
        );
        tty::print("hardware-tests: sb4 seg=");
        tty::println(segment_label(segment as u8));
    }
    out
}

// ---- The capture itself --------------------------------------------------

/// Configure, sync, key, snapshot. Blocks for up to two half-periods
/// (~12 ms) inside one frame, which the frame loop absorbs the same way it
/// absorbs the other probes' blocking uploads.
fn run_segment(segment: usize) -> Snapshot {
    Voice::release(all_used_mask());
    Voice::set_noise_mask(0);

    let (voice, ring) = match segment {
        4 => (Voice::new(3), RING_V3),
        _ => (Voice::new(1), RING_V1),
    };
    // VOICE3 runs at HALF volume against V1's quarter: identical rings mean
    // the tap is pre-volume, a 2x ring means post-volume. The v1.16 data
    // could not distinguish them because both voices used the same volume.
    if segment == 4 {
        voice.set_volume(Volume::linear(1, 2), Volume::linear(1, 2));
    } else {
        voice.set_volume(TONE_VOLUME, TONE_VOLUME);
    }
    match segment {
        1 => {
            voice.set_pitch(Pitch::raw(IMPULSE_PITCH));
            voice.set_start_addr(SpuAddr::new(SPU_IMPULSE_ADDR));
            voice.set_adsr(FLAT_ADSR);
        }
        2 => {
            voice.set_pitch(Pitch::raw(UNITY_PITCH));
            voice.set_start_addr(SpuAddr::new(SPU_SQUARE_ADDR));
            voice.set_adsr(RAMP_ADSR);
        }
        3 => {
            voice.set_pitch(Pitch::raw(UNITY_PITCH));
            voice.set_start_addr(SpuAddr::new(SPU_SQUARE_ADDR));
            voice.set_adsr(FLAT_ADSR);
            Voice::set_noise_mask(voice.mask());
            spu::set_noise_clock(NOISE_SHIFT, NOISE_STEP);
        }
        _ => {
            voice.set_pitch(Pitch::raw(UNITY_PITCH));
            voice.set_start_addr(SpuAddr::new(SPU_SQUARE_ADDR));
            voice.set_adsr(FLAT_ADSR);
        }
    }

    // Key on the half-flag edge, so sample 0 of the captured half is the
    // key-on tick. A timed-out wait marks the row rather than hanging.
    let Some(half) = wait_half_edge() else {
        return timeout_snapshot();
    };
    Voice::start(voice.mask());
    if wait_half_value(!half).is_none() {
        return timeout_snapshot();
    }

    // The writer is in the other half now; the keyed half is stable for
    // 5.8 ms, orders of magnitude past this read.
    let base = ring + if half { HALF_BYTES as u32 } else { 0 };
    let mut words = [0u32; HALF_WORDS];
    spu_dma_read(base, &mut words);

    let stat = unsafe { psx_io::read_u16(psx_hw::spu::SPUSTAT) };
    let envx = unsafe { psx_io::read_u16(psx_hw::spu::BASE + voice.index() as u32 * 16 + 12) };

    let mut raw = [0i16; RAW_SAMPLES];
    let mut first = 0xFFFFu16;
    let mut index = 0usize;
    while index < HALF_SAMPLES {
        let word = words[index / 2];
        let half_word = if index.is_multiple_of(2) {
            word & 0xFFFF
        } else {
            word >> 16
        } as u16;
        let sample = half_word as i16;
        if index < RAW_SAMPLES {
            raw[index] = sample;
        }
        if first == 0xFFFF && sample != 0 {
            first = index as u16;
        }
        index += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, HALF_BYTES) };
    Snapshot {
        stat,
        envx,
        first,
        hash: crc32(bytes),
        raw,
    }
}

fn timeout_snapshot() -> Snapshot {
    let mut snap = Snapshot::empty();
    // Bit 15 of the stat field flags the timeout; real SPUSTAT bit 15 is
    // part of the capture-half field and never set alone with all else zero.
    snap.stat = 0x8000;
    snap.first = 0xFFFF;
    snap
}

fn current_half() -> bool {
    unsafe { psx_io::read_u16(psx_hw::spu::SPUSTAT) & STAT_HALF != 0 }
}

/// Wait for the half flag to move at all; returns the NEW value.
fn wait_half_edge() -> Option<bool> {
    let initial = current_half();
    let mut spins = 0u32;
    while current_half() == initial {
        spins += 1;
        if spins > POLL_BOUND {
            return None;
        }
    }
    Some(!initial)
}

/// Wait until the half flag equals `target`.
fn wait_half_value(target: bool) -> Option<()> {
    let mut spins = 0u32;
    while current_half() != target {
        spins += 1;
        if spins > POLL_BOUND {
            return None;
        }
    }
    Some(())
}

fn all_used_mask() -> u32 {
    Voice::new(1).mask() | Voice::new(3).mask()
}

// ---- Sources -------------------------------------------------------------

/// Square: one cycle per 28 samples, the same arithmetic tone SB2 proved.
/// Impulse: one +0x3800 sample per block, silence elsewhere. Both tables
/// self-loop, block 0 carrying loop-start and the last block end+repeat.
fn upload_tables() {
    let mut square = [0u8; TABLE_BYTES];
    let mut impulse = [0u8; TABLE_BYTES];
    for block in 0..TABLE_BLOCKS {
        let at = block * 16;
        square[at] = TABLE_SHIFT;
        impulse[at] = TABLE_SHIFT;
        let flags = if block == 0 {
            0x04
        } else if block == TABLE_BLOCKS - 1 {
            0x03
        } else {
            0x00
        };
        square[at + 1] = flags;
        impulse[at + 1] = flags;
        for sample in 0..28 {
            let nibble = if sample < 14 { 0x7u8 } else { 0x8 };
            let byte = at + 2 + sample / 2;
            if sample % 2 == 0 {
                square[byte] |= nibble;
            } else {
                square[byte] |= nibble << 4;
            }
        }
        // Impulse: only sample 0 of each block is nonzero.
        impulse[at + 2] |= 0x7;
    }
    spu::upload_adpcm(SpuAddr::new(SPU_SQUARE_ADDR), &square);
    spu::upload_adpcm(SpuAddr::new(SPU_IMPULSE_ADDR), &impulse);
}

fn segment_label(segment: u8) -> &'static str {
    match segment {
        0 => "SQUARE",
        1 => "IMPULSE",
        2 => "ENVRAMP",
        3 => "NOISE",
        4 => "VOICE3",
        _ => "?",
    }
}
