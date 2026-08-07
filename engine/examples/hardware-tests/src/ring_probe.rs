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

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::photo::crc32;
use crate::{hex2, hex8, spu_dma_read};

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
/// Frames of audible tone per segment, then the gap. The capture itself
/// happens inside frame 0; the tail is an operator cue, not the instrument.
const TONE_FRAMES: u16 = 60;
const GAP_FRAMES: u16 = 30;

// ---- Payload -------------------------------------------------------------

/// Raw samples carried per segment beside the full-half CRC. 32 is enough to
/// see the interpolation kernel, the attack's first steps, the LFSR's first
/// words, and the key-on latency, while the hash still covers all 256.
const RAW_SAMPLES: usize = 32;
/// magic, schema, segment count, raw count, run, noise shift, noise step, pad.
const HEADER_BYTES: usize = 8;
/// stat, envx, first-nonzero, CRC of the half, then the raw window.
const SEGMENT_BYTES: usize = 2 + 2 + 2 + 4 + RAW_SAMPLES * 2;
const BINARY_LEN: usize = HEADER_BYTES + SEGMENTS * SEGMENT_BYTES + 4;
const BASE64_LEN: usize = BINARY_LEN.div_ceil(3) * 4;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

const QR_VERSION_NUM: u8 = 19;
const QR_VERSION: Version = Version::new(QR_VERSION_NUM);
const QR_SIZE: usize = 4 * QR_VERSION_NUM as usize + 17;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
/// Byte-mode capacity at ECC Medium for version 19, from the ISO table. The
/// assert is what stops a new segment being discovered on a burn.
const QR_BYTE_CAPACITY: usize = 624;
const _: () = assert!(QR_TEXT_MAX <= QR_BYTE_CAPACITY, "SB4 QR payload too big");
const _: () = assert!(
    (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE <= 240,
    "SB4 QR too tall"
);

// Shift 13 clocks the LFSR fast enough that the 32 raw window words read
// out an actual sequence; at the old shift 8 / step 2 the LFSR stepped only
// every ~128 ticks and the window saw a constant on both platforms. The
// payload header carries shift/step, so decoders need no flag day.
const NOISE_SHIFT: u8 = 13;
const NOISE_STEP: u8 = 2;

/// One captured half, as the payload will carry it.
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

pub(crate) struct RingProbe {
    segment: u8,
    frame: u16,
    complete: bool,
    run: u8,
    snapshots: [Snapshot; SEGMENTS],
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl RingProbe {
    pub(crate) const fn new() -> Self {
        Self {
            segment: 0,
            frame: 0,
            complete: false,
            run: 0,
            snapshots: [Snapshot::empty(); SEGMENTS],
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn start(&mut self) {
        self.segment = 0;
        self.frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.snapshots = [Snapshot::empty(); SEGMENTS];
        self.qr_size = 0;
        // Warm-console discipline: this runs from the demo disc's menu, after
        // whatever the launcher and other probes left behind. Own everything.
        spu::init();
        spu::set_main_volume(Volume::HALF, Volume::HALF);
        spu::enable_cd_audio(false);
        upload_tables();
        tty::println("hardware-tests: sb4 begin capture-ring readback");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    pub(crate) fn update(&mut self, _tick: u32) {
        if self.complete {
            return;
        }
        if self.frame == 0 {
            let segment = self.segment as usize;
            self.snapshots[segment] = run_segment(segment);
            tty::print("hardware-tests: sb4 seg=");
            tty::print(hex2(self.segment).as_str());
            tty::print(" ");
            tty::println(segment_label(self.segment));
        }
        self.frame = self.frame.saturating_add(1);
        if self.frame == TONE_FRAMES {
            Voice::key_off(all_used_mask());
            Voice::set_noise_mask(0);
        }
        if self.frame >= TONE_FRAMES + GAP_FRAMES {
            self.frame = 0;
            if (self.segment as usize) + 1 < SEGMENTS {
                self.segment += 1;
            } else {
                self.complete = true;
                self.encode_qr();
                self.print_payload();
            }
        }
    }

    /// The payload bytes, CRC included. Schema note: schema 1 fixes the
    /// noise clock at shift 8 step 2; bump the schema if that ever changes,
    /// because a captured LFSR row is meaningless without its clock.
    fn build_binary(&self) -> [u8; BINARY_LEN] {
        let mut binary = [0u8; BINARY_LEN];
        binary[..4].copy_from_slice(b"SB4B");
        binary[4] = 1;
        binary[5] = SEGMENTS as u8;
        binary[6] = RAW_SAMPLES as u8;
        binary[7] = self.run;
        let mut at = HEADER_BYTES;
        for snap in &self.snapshots {
            binary[at..at + 2].copy_from_slice(&snap.stat.to_le_bytes());
            binary[at + 2..at + 4].copy_from_slice(&snap.envx.to_le_bytes());
            binary[at + 4..at + 6].copy_from_slice(&snap.first.to_le_bytes());
            binary[at + 6..at + 10].copy_from_slice(&snap.hash.to_le_bytes());
            let mut raw_at = at + 10;
            for sample in &snap.raw {
                binary[raw_at..raw_at + 2].copy_from_slice(&sample.to_le_bytes());
                raw_at += 2;
            }
            at += SEGMENT_BYTES;
        }
        let crc = crc32(&binary[..BINARY_LEN - 4]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        binary
    }

    fn encode_qr(&mut self) {
        let binary = self.build_binary();
        self.binary_crc = u32::from_le_bytes([
            binary[BINARY_LEN - 4],
            binary[BINARY_LEN - 3],
            binary[BINARY_LEN - 2],
            binary[BINARY_LEN - 1],
        ]);

        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        let mut text = [0u8; QR_TEXT_MAX];
        let mut len = 0usize;
        append(&mut text, &mut len, b"SB4/");
        append(&mut text, &mut len, &payload);
        append(&mut text, &mut len, b"/C:");
        append(
            &mut text,
            &mut len,
            hex8(self.binary_crc).digits().as_bytes(),
        );
        let encoded = unsafe { core::str::from_utf8_unchecked(&text[..len]) };
        let mut temp = [0u8; QR_BUFFER_LEN];
        let mut output = [0u8; QR_BUFFER_LEN];
        let Ok(qr) = QrCode::encode_text(
            encoded,
            &mut temp,
            &mut output,
            QrCodeEcc::Medium,
            QR_VERSION,
            QR_VERSION,
            None,
            false,
        ) else {
            self.qr_size = 0;
            return;
        };
        self.qr_modules.fill(0);
        self.qr_size = qr.size() as u8;
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                if qr.get_module(x, y) {
                    let bit = y as usize * QR_SIZE + x as usize;
                    self.qr_modules[bit / 8] |= 1 << (bit & 7);
                }
            }
        }
    }

    /// The whole payload to the TTY, so a headless emulator run diffs this
    /// probe without a QR in the loop. The QR is for silicon.
    fn print_payload(&self) {
        let binary = self.build_binary();
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: sb4 SB4/");
        // SAFETY: base64 output is ASCII.
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        if !self.complete {
            font.draw_text(8, 30, "CAPTURE-RING READBACK", (150, 170, 200));
            font.draw_text(8, 44, "SEG", (140, 160, 190));
            font.draw_text(48, 44, hex2(self.segment).as_str(), (232, 236, 244));
            font.draw_text(72, 44, segment_label(self.segment), (96, 200, 255));
            font.draw_text(8, 58, segment_description(self.segment), (220, 224, 230));
            // Completed rows so far: latency and hash, the two numbers an
            // operator can compare against a previous run on the spot.
            let mut y = 80;
            for (index, snap) in self.snapshots.iter().enumerate() {
                if index >= self.segment as usize {
                    break;
                }
                font.draw_text(8, y, segment_label(index as u8), (140, 160, 190));
                font.draw_text(80, y, "T+", (140, 160, 190));
                font.draw_text(100, y, hex8(snap.first as u32).digits(), (232, 236, 244));
                font.draw_text(180, y, hex8(snap.hash).digits(), (96, 200, 255));
                y += 12;
            }
            return;
        }

        font.draw_text(8, 30, "COMPLETE - PHOTOGRAPH THE QR", (96, 240, 128));
        font.draw_text(8, 228, "X RERUN", (150, 170, 200));
        if self.qr_size as usize != QR_SIZE {
            font.draw_text(88, 112, "QR ENCODE FAILED", (255, 96, 96));
            return;
        }
        let total = (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE;
        let left = (320 - total) / 2;
        let top = 44;
        gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
        let data_left = left + QR_QUIET * QR_SCALE;
        let data_top = top + QR_QUIET * QR_SCALE;
        for y in 0..QR_SIZE {
            let mut x = 0usize;
            while x < QR_SIZE {
                while x < QR_SIZE && !self.qr_module(x, y) {
                    x += 1;
                }
                let first = x;
                while x < QR_SIZE && self.qr_module(x, y) {
                    x += 1;
                }
                if first < x {
                    gpu::draw_rect_flat(
                        data_left + first as i16 * QR_SCALE,
                        data_top + y as i16 * QR_SCALE,
                        ((x - first) as i16 * QR_SCALE) as u16,
                        QR_SCALE as u16,
                        0,
                        0,
                        0,
                    );
                }
            }
        }
    }
}

// ---- The capture itself --------------------------------------------------

/// Configure, sync, key, snapshot. Blocks for up to two half-periods
/// (~12 ms) inside one frame, which the frame loop absorbs the same way it
/// absorbs the other probes' blocking uploads.
fn run_segment(segment: usize) -> Snapshot {
    Voice::key_off(all_used_mask());
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
    Voice::key_on(voice.mask());
    if wait_half_value(!half).is_none() {
        return timeout_snapshot();
    }

    // The writer is in the other half now; the keyed half is stable for
    // 5.8 ms, orders of magnitude past this read.
    let base = ring + if half { HALF_BYTES as u32 } else { 0 };
    let mut words = [0u32; HALF_WORDS];
    spu_dma_read(base, &mut words);

    let stat = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) };
    let envx = unsafe { psx_io::read16(psx_io::spu::SPU_BASE + voice.index() as u32 * 16 + 12) };

    let mut raw = [0i16; RAW_SAMPLES];
    let mut first = 0xFFFFu16;
    let mut index = 0usize;
    while index < HALF_SAMPLES {
        let word = words[index / 2];
        let half_word = if index % 2 == 0 {
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
    unsafe { psx_io::read16(psx_io::spu::SPUSTAT) & STAT_HALF != 0 }
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

fn segment_description(segment: u8) -> &'static str {
    match segment {
        0 => "V1 SQUARE: DECODE + KEY-ON LATENCY",
        1 => "V1 HALF PITCH: INTERPOLATION KERNEL",
        2 => "V1 SLOW ATTACK: ENVELOPE STEPPING",
        3 => "V1 NOISE ON: LFSR SEQUENCE",
        4 => "V3 SQUARE: THE OTHER RING",
        _ => "?",
    }
}

// ---- Local transport helpers, per the module pattern ---------------------

fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let end = (*len + bytes.len()).min(target.len());
    let take = end - *len;
    target[*len..end].copy_from_slice(&bytes[..take]);
    *len = end;
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    let mut out = 0usize;
    let mut index = 0usize;
    while index < input.len() {
        let b0 = input[index] as u32;
        let b1 = if index + 1 < input.len() {
            input[index + 1] as u32
        } else {
            0
        };
        let b2 = if index + 2 < input.len() {
            input[index + 2] as u32
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        output[out] = BASE64[(triple >> 18) as usize & 63];
        output[out + 1] = BASE64[(triple >> 12) as usize & 63];
        output[out + 2] = if index + 1 < input.len() {
            BASE64[(triple >> 6) as usize & 63]
        } else {
            b'='
        };
        output[out + 3] = if index + 2 < input.len() {
            BASE64[triple as usize & 63]
        } else {
            b'='
        };
        out += 4;
        index += 3;
    }
    out
}
