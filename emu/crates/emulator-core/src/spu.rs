//! SPU -- Sound Processing Unit.
//!
//! The SPU is a 24-voice ADPCM sample engine with per-voice ADSR
//! envelopes, pitch-controlled playback, a 512 KiB sample RAM, and
//! stereo output mixed at 44.1 kHz. This module is the real thing --
//! ADPCM decode, voice state machine, ADSR, stereo mixing. What's
//! explicitly **not** modelled yet (each a follow-up session):
//!
//! - XA ADPCM streaming (CD-DA audio / in-game speech) is decoded by
//!   the CDROM module; the SPU exposes [`xa_decode_block`] for it but
//!   does not call it from `tick_sample`.
//!
//! Already in: 1024-entry Gaussian sample interpolation (`GAUSS_TABLE`)
//! and hardware-accurate voice/main volume decode: fixed signed-Q15
//! levels (bit15=0, including negative phase-inverted volumes) and a
//! fully animated sweep envelope (bit15=1), matching PSX-SPX and
//! the PSX hardware volume-sweep behaviour.
//!
//! Reference implementations consulted as parity oracles:
//! - PCSX-Redux `src/spu/{spu,adsr,registers,dma}.cc` (GPL-2.0-or-later)
//!   -- behavioural reference for ADSR rate tables and voice state model.
//! - psx-spx "SPU" chapter for register layout + ADPCM filter table.
//! - Neill Corlett's SPU envelope notes (quoted in `adsr.cc`).
//!
//! Pipeline per 44.1 kHz sample:
//!
//! 1. For each voice, skip `Off` envelopes, advance ADPCM read
//!    position by `raw_pitch / 0x1000` of a sample, decode the next
//!    16-byte ADPCM block when `sample_index` reaches 28, apply loop
//!    flags, interpolate at the fractional position, advance ADSR,
//!    then apply per-voice L/R volume.
//! 2. Sum all 24 voices into `(sum_l, sum_r)`.
//! 3. Feed EON-enabled voices and CD reverb input into the Neill/Redux
//!    reverb network in SPU RAM.
//! 4. For PCSX-Redux parity, route the dry voice/CD sum directly to
//!    output; Redux stores but does not apply main-volume writes in
//!    its SPU mixer. Then add wet reverb output and saturate to i16.
//! 5. Push (l, r) to the host-facing output ring.
//!
//! SPU IRQ: if SPUCNT bit 6 (IRQEnable) is set and the IRQ address
//! matches a voice sample-read pointer, a transfer FIFO access, or
//! Redux's decoded/capture-buffer cursor in low SPU RAM, we latch
//! STATUS bit 6 and signal the bus.
//!
//! Sample rate: 44_100 Hz. PSX clock is 33_868_800 Hz, so 1 sample =
//! 768 cycles. We tick the SPU from the scheduler every [`SAMPLE_CYCLES`]
//! cycles.
//!
//! ## Provenance
//!
//! Portions of this module are parity-matched against, and in places
//! derived from, PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later. Points of
//! correspondence are flagged inline with `Redux` references. PSoXide is
//! released under GPL-2.0-or-later in part to honor this lineage; see
//! `LICENSE` and `docs/license-audit.md`.

use crate::scheduler::{EventSlot, Scheduler};

mod xa;
pub use xa::{xa_decode_block, XaDecoderState};

// ===============================================================
//  Register addresses -- voice bank + global + reverb config.
// ===============================================================

/// Base of the SPU MMIO window. 512 bytes total spanning voice bank,
/// global control regs, and reverb coefficient registers.
pub const SPU_BASE: u32 = 0x1F80_1C00;
/// One past the end of the SPU MMIO window.
pub const SPU_END: u32 = 0x1F80_1E00;

/// Base of the 24-voice register bank (16 bytes per voice).
pub const VOICE_BASE: u32 = 0x1F80_1C00;
/// One past the end of the voice bank (24 * 16 = 0x180 bytes → 0x1F80_1D80).
pub const VOICE_END: u32 = 0x1F80_1D80;

/// Main Volume Left (16-bit, Q14).
pub const MAIN_VOL_L: u32 = 0x1F80_1D80;
/// Main Volume Right (16-bit, Q14).
pub const MAIN_VOL_R: u32 = 0x1F80_1D82;
/// Reverb output volume Left.
pub const REVERB_VOL_L: u32 = 0x1F80_1D84;
/// Reverb output volume Right.
pub const REVERB_VOL_R: u32 = 0x1F80_1D86;
/// Key-On low (voices 0..15).
pub const KON_LO: u32 = 0x1F80_1D88;
/// Key-On high (voices 16..23).
pub const KON_HI: u32 = 0x1F80_1D8A;
/// Key-Off low (voices 0..15).
pub const KOFF_LO: u32 = 0x1F80_1D8C;
/// Key-Off high (voices 16..23).
pub const KOFF_HI: u32 = 0x1F80_1D8E;
/// Pitch modulation enable low.
pub const PMON_LO: u32 = 0x1F80_1D90;
/// Pitch modulation enable high.
pub const PMON_HI: u32 = 0x1F80_1D92;
/// Noise mode enable low.
pub const NON_LO: u32 = 0x1F80_1D94;
/// Noise mode enable high.
pub const NON_HI: u32 = 0x1F80_1D96;
/// Reverb enable low.
pub const EON_LO: u32 = 0x1F80_1D98;
/// Reverb enable high.
pub const EON_HI: u32 = 0x1F80_1D9A;
/// ENDX low (per-voice "reached loop-end block" latch, write-1-to-clear).
pub const ENDX_LO: u32 = 0x1F80_1D9C;
/// ENDX high.
pub const ENDX_HI: u32 = 0x1F80_1D9E;
/// Reverb work-area start address (halfword, scaled by 8 → byte addr).
pub const REVERB_BASE: u32 = 0x1F80_1DA2;
/// IRQ address (halfword * 8 = byte addr into SPU RAM).
pub const IRQ_ADDR: u32 = 0x1F80_1DA4;
/// Data transfer address (halfword * 8 = byte addr into SPU RAM).
pub const TRANSFER_ADDR: u32 = 0x1F80_1DA6;
/// Data transfer FIFO (reads pop, writes push at TRANSFER_ADDR, which advances).
pub const TRANSFER_FIFO: u32 = 0x1F80_1DA8;
/// SPU control register.
pub const SPUCNT: u32 = 0x1F80_1DAA;
/// SPUCNT bit 0: route CD-DA / XA-ADPCM input through the SPU mixer.
const SPUCNT_CD_AUDIO_ENABLE: u16 = 1 << 0;
/// SPUCNT bit 2: route CD-DA / XA-ADPCM input into the reverb engine.
const SPUCNT_CD_REVERB_ENABLE: u16 = 1 << 2;
/// SPUCNT bit 7: enable reverb steady-state processing.
const SPUCNT_REVERB_MASTER_ENABLE: u16 = 1 << 7;
/// SPUCNT bit 14: 0 = muted, 1 = unmuted.
const SPUCNT_UNMUTE: u16 = 1 << 14;
/// Data transfer control (typically 0x0004 -- 4-bit transfer step).
pub const TRANSFER_CTRL: u32 = 0x1F80_1DAC;
/// SPU status register.
pub const SPUSTAT: u32 = 0x1F80_1DAE;
/// CD audio input volume Left.
pub const CD_VOL_L: u32 = 0x1F80_1DB0;
/// CD audio input volume Right.
pub const CD_VOL_R: u32 = 0x1F80_1DB2;
/// External audio input volume Left.
pub const EXT_VOL_L: u32 = 0x1F80_1DB4;
/// External audio input volume Right.
pub const EXT_VOL_R: u32 = 0x1F80_1DB6;
/// Current Main Volume Left backing register.
pub const CURRENT_MAIN_VOL_L: u32 = 0x1F80_1DB8;
/// Current Main Volume Right backing register.
pub const CURRENT_MAIN_VOL_R: u32 = 0x1F80_1DBA;

/// Start of reverb configuration area (32 × 16-bit coefficient regs).
pub const REVERB_CFG_BASE: u32 = 0x1F80_1DC0;

/// Per-voice offsets within the 16-byte voice block.
#[allow(dead_code)]
mod voice_offset {
    /// +0..1 volume left (Q14, or sweep config if bit 15 set).
    pub const VOLUME_L: u32 = 0x0;
    /// +2..3 volume right.
    pub const VOLUME_R: u32 = 0x2;
    /// +4..5 ADPCM pitch. `0x1000` = base rate (44.1 kHz). 16-bit R/W
    /// register (full readback); the effective rate clamps to 0x3FFF.
    pub const PITCH: u32 = 0x4;
    /// +6..7 ADPCM start address (in 8-byte units; <<3 = byte addr).
    pub const START_ADDR: u32 = 0x6;
    /// +8..9 ADSR config low -- attack mode + rate + decay rate + sustain level.
    pub const ADSR_LO: u32 = 0x8;
    /// +A..B ADSR config high -- sustain mode + sustain rate + release mode + release rate.
    pub const ADSR_HI: u32 = 0xA;
    /// +C..D Current ADSR volume (read-only; returns current envelope level).
    pub const ADSR_CURRENT: u32 = 0xC;
    /// +E..F Repeat (loop) address (in 8-byte units).
    pub const REPEAT_ADDR: u32 = 0xE;
}

// ===============================================================
//  Sizing / timing constants.
// ===============================================================

/// Number of voices in the PSX SPU.
pub const NUM_VOICES: usize = 24;

/// SPU RAM size in bytes (512 KiB).
pub const SPU_RAM_BYTES: usize = 512 * 1024;
/// SPU RAM size in 16-bit words.
pub const SPU_RAM_HALFWORDS: usize = SPU_RAM_BYTES / 2;

/// System clock cycles per SPU sample. 33_868_800 Hz / 44_100 Hz = 768.
pub const SAMPLE_CYCLES: u64 = 768;

/// ADPCM block size in bytes (1 header + 1 flags + 14 data bytes).
pub const ADPCM_BLOCK_BYTES: usize = 16;
/// Samples produced per ADPCM block.
pub const ADPCM_SAMPLES_PER_BLOCK: usize = 28;

/// Host-facing audio output buffer cap. Frontend drains periodically;
/// if it falls behind we discard the oldest samples.
const OUTPUT_BUFFER_CAP: usize = 44100 * 2; // 2 seconds of stereo samples

// ===============================================================
//  ADPCM filter table (5 filters × 2 coefficients, matches PSX-SPX).
// ===============================================================

/// ADPCM prediction filter coefficients: `(s_1_weight, s_2_weight)` in Q6.
///
/// Applied as `fa = raw + (s_1 * f[0] + s_2 * f[1]) >> 6` during block decode.
/// Filters 0..4 are the canonical set used by the real hardware; filters 5..15
/// use the same coefficients (SPU-SPX notes that only the lower 3 bits of the
/// predictor field matter, so 0..7 clamp to 0..4 -- we clamp explicitly).
const ADPCM_FILTER_TABLE: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

// ===============================================================
//  ADSR envelope rate tables. Generated to match Redux's
//  `EnvelopeTables` output exactly so envelope ticks line up
//  sample-for-sample against the parity oracle.
// ===============================================================

/// Envelope tick-period denominator for each rate (0..=127).
///
/// Rate < 48: 1 (increment/decrement happens every sample).
/// Rate >= 48: `1 << ((rate >> 2) - 11)` (doubles every 4 rate units).
const fn envelope_denominator(rate: usize) -> i32 {
    if rate < 48 {
        1
    } else {
        1i32 << ((rate >> 2) - 11)
    }
}

/// Envelope positive increment numerator: applied on increment ticks.
const fn envelope_numerator_increase(rate: usize) -> i32 {
    let step = 7 - (rate as i32 & 3);
    if rate < 48 {
        step << (11 - (rate >> 2))
    } else {
        step
    }
}

/// Envelope negative decrement numerator.
const fn envelope_numerator_decrease(rate: usize) -> i32 {
    let step = -8 + (rate as i32 & 3);
    if rate < 48 {
        step << (11 - (rate >> 2))
    } else {
        step
    }
}

// ===============================================================
//  ADSR phase + per-voice envelope state.
// ===============================================================

/// ADSR state-machine phase. Voices start at `Off`; KON transitions to
/// `Attack` and resets the envelope. `Off` voices contribute silence.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum AdsrPhase {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Decoded ADSR configuration -- parsed from the 32-bit `(adsr_lo | adsr_hi<<16)`
/// register pair. We decode once at write time and stash the bit-fields so the
/// hot path (envelope tick per sample) is pure arithmetic.
#[derive(Copy, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct AdsrConfig {
    attack_rate: i32,   // 0..=127 (with mode bit folded in)
    attack_exp: bool,   // linear vs exponential slope
    decay_rate: i32,    // 0..=15
    sustain_level: i32, // 0..=15 (target = (N+1) * 0x800)
    sustain_rate: i32,  // 0..=127 (with mode bits folded in)
    sustain_exp: bool,
    sustain_increase: bool, // 1 = rising, 0 = falling
    release_rate: i32,      // 0..=31
    release_exp: bool,
}

/// Parse the low 16 bits of the ADSR register pair into `(ar, ar_exp, dr, sl)`.
fn parse_adsr_lo(lo: u16, cfg: &mut AdsrConfig) {
    cfg.attack_exp = (lo & (1 << 15)) != 0;
    cfg.attack_rate = ((lo >> 8) & 0x7F) as i32;
    cfg.decay_rate = ((lo >> 4) & 0xF) as i32;
    cfg.sustain_level = (lo & 0xF) as i32;
}

/// Parse the high 16 bits of the ADSR register pair into `(sm, sd, sr, rm, rr)`.
fn parse_adsr_hi(hi: u16, cfg: &mut AdsrConfig) {
    cfg.sustain_exp = (hi & (1 << 15)) != 0;
    cfg.sustain_increase = (hi & (1 << 14)) == 0;
    cfg.sustain_rate = ((hi >> 6) & 0x7F) as i32;
    cfg.release_exp = (hi & (1 << 5)) != 0;
    cfg.release_rate = (hi & 0x1F) as i32;
}

// ===============================================================
//  Voice state.
// ===============================================================

/// One SPU volume channel. Holds the raw 16-bit register value so
/// reads round-trip verbatim, plus the hardware `current` level in
/// full signed Q15 (-0x8000..=0x7FFF). Voice/main volume registers
/// support a fixed level (bit15=0, signed 15-bit value * 2, so bit14
/// is a genuine negative phase) and an animated sweep envelope
/// (bit15=1, ramped linear/exponential up/down each sample). CD,
/// external, and reverb-output volumes are plain fixed signed values
/// written via [`write_signed_q15`]. The sweep math is a direct port
/// of the PSX hardware volume sweep
/// and the per-voice volume envelope;
/// the level is applied as `(sample * current) >> 15`.
#[derive(Copy, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct VolumeEnvelope {
    /// The last 16-bit word written to the register. `reads` echo
    /// this so software verification paths see the exact config.
    raw: u16,
    /// Current signed Q15 level, -0x8000..=0x7FFF. Applied to samples
    /// as `(sample * current) >> 15`, matching both parity oracles.
    current: i16,
    /// True only while a sweep (bit15=1) is animating; `tick` is a
    /// no-op for fixed levels and for never-ticking rates.
    sweep_active: bool,
    /// Sweep envelope state (mirrors PSX-SPX `VolumeEnvelope`).
    sweep_counter: u32,
    /// Counter increment. MUST be u32: the rate-0x7F "never ticks" case
    /// shifts 0x8000 right by 20 (`(0x7F>>2)-11`), which a u16 would mask
    /// to `>>4`=0x0800 (a nonzero, wrongly-active increment) in release
    /// builds. A wider integer width is what makes the over-shift collapse
    /// to 0 as the PSX hardware requires.
    sweep_increment: u32,
    sweep_step: i16,
    sweep_rate: u8,
    sweep_decreasing: bool,
    sweep_exponential: bool,
    sweep_phase_invert: bool,
}

impl VolumeEnvelope {
    fn new() -> Self {
        Self::default()
    }

    /// Accept a new 16-bit voice/main volume register value.
    ///
    /// bit15=0: fixed volume. Bits 0..14 are a signed 15-bit value
    /// representing Volume/2, so `current = signed15 * 2` (a set bit14
    /// is a real negative/phase-inverted volume). bit15=1: configure a
    /// sweep envelope and leave `current` at its prior level, ramping
    /// from there each `tick`. Matches PSX-SPX
    /// the old PCSX-Redux-style fabricated
    /// sweep gain + sign-masked fixed level are gone (SPU_AUDIT #5/#6/#14).
    fn write(&mut self, raw: u16) {
        self.raw = raw;
        if raw & 0x8000 != 0 {
            // Sweep mode: program the envelope, keep current_level.
            self.sweep_reset(
                (raw & 0x7F) as u8,
                raw & (1 << 13) != 0, // decreasing
                raw & (1 << 14) != 0, // exponential
                raw & (1 << 12) != 0, // phase-invert
            );
            self.sweep_active = self.sweep_increment > 0;
        } else {
            // Fixed mode: signed 15-bit field (bits 0..14) * 2.
            let field = ((raw & 0x7FFF) as i16) << 1 >> 1; // sign-extend bit14
            self.current = field.wrapping_mul(2);
            self.sweep_active = false;
        }
    }

    /// Accept a fixed signed Q15 volume register. CD input, external
    /// input, and reverb output volumes are plain signed volumes, not
    /// voice/main sweep registers with bit-14 phase semantics.
    fn write_signed_q15(&mut self, raw: u16) {
        self.raw = raw;
        self.current = raw as i16;
        self.sweep_active = false;
    }

    /// Configure the sweep envelope. Direct port of PSX-SPX
    /// `VolumeEnvelope::Reset(rate, rate_mask=0x7F, ...)` /
    /// PSX-SPX `Envelope::reset`.
    fn sweep_reset(&mut self, rate: u8, decreasing: bool, exponential: bool, phase_invert: bool) {
        self.sweep_rate = rate;
        self.sweep_decreasing = decreasing;
        self.sweep_exponential = exponential;
        // psx-spx: phase bit has no effect in exponential-decrease mode.
        self.sweep_phase_invert = phase_invert && !(decreasing && exponential);
        self.sweep_counter = 0;
        self.sweep_increment = 0x8000;

        let base_step = 7 - (rate as i32 & 3);
        let neg = (decreasing ^ phase_invert) || (decreasing && exponential);
        let mut step = if neg { !base_step } else { base_step };
        if rate < 44 {
            step <<= 11 - (rate >> 2);
        } else if rate >= 48 {
            self.sweep_increment >>= (rate >> 2) - 11;
            // Rate 0x7F (all bits set under the 0x7F mask) never ticks.
            if (rate & 0x7F) != 0x7F {
                self.sweep_increment = self.sweep_increment.max(1);
            }
        }
        self.sweep_step = step as i16;
    }

    /// Advance one SPU sample. Ramps `current` per the configured
    /// sweep; a no-op for fixed levels (`sweep_active == false`).
    /// Direct port of PSX-SPX `VolumeEnvelope::Tick`.
    fn tick(&mut self) {
        if !self.sweep_active {
            return;
        }
        let mut this_increment = self.sweep_increment;
        let mut this_step = self.sweep_step as i32;
        if self.sweep_exponential {
            if self.sweep_decreasing {
                this_step = (this_step * self.current as i32) >> 15;
            } else if self.current >= 0x6000 {
                if self.sweep_rate < 40 {
                    this_step >>= 2;
                } else if self.sweep_rate >= 44 {
                    this_increment >>= 2;
                } else {
                    this_step >>= 1;
                    this_increment >>= 1;
                }
            }
        }
        self.sweep_counter += this_increment;
        if self.sweep_counter & 0x8000 == 0 {
            return;
        }
        self.sweep_counter = 0;
        let new_level = self.current as i32 + this_step;
        if !self.sweep_decreasing {
            let clamped = new_level.clamp(-0x8000, 0x7FFF);
            self.current = clamped as i16;
            let limit = if this_step < 0 { -0x8000 } else { 0x7FFF };
            self.sweep_active = clamped != limit;
        } else {
            let clamped = if self.sweep_phase_invert {
                new_level.clamp(-0x8000, 0)
            } else {
                new_level.max(0)
            };
            self.current = clamped as i16;
            self.sweep_active = clamped != 0;
        }
    }

    /// Read-back value -- always returns the raw register the CPU
    /// wrote, not the animated current level.
    fn reg_read(&self) -> u16 {
        self.raw
    }
}

/// Per-voice runtime state. Holds decode buffers, ADSR envelope,
/// volumes, and loop pointers. Kept plain (no padding or SIMD) --
/// 24 copies of this struct live in `Spu::voices` and mix together on
/// each sample.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Voice {
    /// Left volume envelope.
    vol_l: VolumeEnvelope,
    /// Right volume envelope.
    vol_r: VolumeEnvelope,
    /// Raw pitch register, full 16-bit R/W -- reads echo the written value
    /// like hardware. `0x1000` plays at the sample's source rate (typically
    /// 44.1 kHz); the rate counter clamps to 0x3FFF at use.
    raw_pitch: u16,
    /// Byte address into SPU RAM where playback begins on KON. `<<3`
    /// of the register value, 16-byte aligned for the decoder.
    start_addr: u32,
    /// The raw 16-bit START_ADDR register value, kept verbatim so reads
    /// echo it back like hardware (the decoder uses `start_addr`, the
    /// `<<3`/aligned form derived from it).
    start_addr_raw: u16,
    /// Loop address (byte address). Set by software via REPEAT_ADDR
    /// register and by the ADPCM flag-4 bit (loop-start).
    loop_addr: u32,
    /// Raw REPEAT_ADDR register value for readback: the software-written
    /// 16-bit word, or `loop_addr >> 3` when the decoder sets the loop.
    loop_addr_raw: u16,
    /// True if software wrote REPEAT_ADDR directly since voice start;
    /// suppresses the ADPCM flag-4 loop-start auto-update (matches
    /// Redux's `IgnoreLoop`).
    loop_addr_locked: bool,
    /// Raw ADSR_LO / ADSR_HI words. Stored so reads echo them back.
    adsr_lo: u16,
    adsr_hi: u16,
    /// Decoded ADSR parameters.
    adsr: AdsrConfig,
    /// Current ADSR phase.
    phase: AdsrPhase,
    /// Envelope level, 0..=0x7FFF (Q15). Multiplies the decoded sample.
    envelope: i32,
    /// Envelope sub-sample counter (`EnvelopeVolF` in Redux); compared
    /// to `denominator[rate]` to decide whether to step the envelope
    /// this sample.
    envelope_sub: i32,
    /// Current byte address into SPU RAM for the *next* ADPCM block to
    /// decode. Updated after each block consumed.
    current_addr: u32,
    /// Decoded samples from the most recent 16-byte block (28 samples).
    /// Indexed by `sample_index`. Each sample is saturated to i16
    /// (-0x8000..=0x7FFF) at decode time before it is stored here and fed
    /// back into the ADPCM predictor history, matching real hardware and
    /// both parity oracles (the IIR runs on the 16-bit-saturated value).
    sample_buf: [i32; ADPCM_SAMPLES_PER_BLOCK],
    /// Index into `sample_buf`; when it reaches 28 we decode the next
    /// block before taking the next sample.
    sample_index: usize,
    /// Redux-style fixed-point sample cursor (`spos`). Each output
    /// sample consumes decoded input samples while this stays above
    /// `0x10000`, then adds the pitch step (`raw_pitch << 4`) for the
    /// next call. Starting at `0x30000` primes the Gaussian window
    /// with three decoded samples before the first audible output.
    sample_pos: u32,
    /// Rolling 4-sample interpolation ring. Redux stores decoded
    /// samples into `SB[29..32]` and runs the Gaussian window over the
    /// ring so block boundaries still see the previous block's tail.
    /// Without this history, the interpolator falls back to zeros at
    /// every 28-sample ADPCM edge and the output gets audibly gritty.
    interp_ring: [i16; 4],
    /// Next insertion slot in `interp_ring`. Also the logical
    /// "oldest sample" index when reading the Gaussian window.
    interp_pos: usize,
    /// Previous two decoded samples -- ADPCM filter history. Preserved
    /// across block boundaries; reset on KON.
    s_1: i32,
    s_2: i32,
    /// Set when a decoded block had the stop flag without a valid
    /// loop. The current 28-sample block must still play out fully;
    /// Redux only turns the voice off when the decoder reaches the
    /// *next* block boundary.
    stop_after_block: bool,
    /// ENDX latch is deferred: a decoded loop-end (flag-1) block sets
    /// this, and ENDX is latched at the *next* block boundary in
    /// `fetch_voice_sample`, i.e. only after the loop-end block's 28
    /// samples have actually played. PSX-SPX set ENDX at
    /// the boundary crossing, not when the loop-end block is decoded.
    endx_pending: bool,
    /// Number of ADPCM blocks decoded since the last key-on. The first
    /// decoded block is the voice's "first block" (`== 1`); that is the
    /// window in which a REPEAT_ADDR write must NOT lock the loop
    /// address so the sample's own loop-start flag can still override it
    /// (the PSX hardware first-block window).
    decoded_block_count: u32,
    /// Most recent interpolated sample output by this voice (post-ADSR,
    /// pre-volume). Kept for reads of the ADSR_CURRENT register and
    /// pitch modulation consumers.
    last_sample: i16,
    /// Ticks remaining before a freshly keyed voice starts stepping its
    /// envelope and producing output. SB4 silicon 2026-08-07: the first
    /// envelope step lands ~8 ticks after the KON write; the KON-applied-
    /// at-end-of-tick model accounts for 1 of those. Calibrate the exact
    /// constant on the next burn.
    #[serde(default)]
    start_delay: u8,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            vol_l: VolumeEnvelope::new(),
            vol_r: VolumeEnvelope::new(),
            raw_pitch: 0,
            start_addr: 0,
            start_addr_raw: 0,
            loop_addr: 0,
            loop_addr_raw: 0,
            loop_addr_locked: false,
            adsr_lo: 0,
            adsr_hi: 0,
            adsr: AdsrConfig::default(),
            phase: AdsrPhase::Off,
            envelope: 0,
            envelope_sub: 0,
            current_addr: 0,
            sample_buf: [0; ADPCM_SAMPLES_PER_BLOCK],
            sample_index: ADPCM_SAMPLES_PER_BLOCK, // forces decode on first tick
            sample_pos: 0x30000,
            interp_ring: [0; 4],
            interp_pos: 0,
            s_1: 0,
            s_2: 0,
            stop_after_block: false,
            endx_pending: false,
            decoded_block_count: 0,
            last_sample: 0,
            start_delay: 0,
        }
    }
}

impl Voice {
    /// Reset envelope + decode state on KON (key-on). The voice will
    /// start decoding from `start_addr` on the next sample tick.
    fn key_on(&mut self) {
        self.phase = AdsrPhase::Attack;
        self.start_delay = 7;
        self.envelope = 0;
        self.envelope_sub = 0;
        self.current_addr = self.start_addr;
        self.sample_index = ADPCM_SAMPLES_PER_BLOCK;
        self.sample_pos = 0x30000;
        self.interp_ring = [0; 4];
        self.interp_pos = 0;
        self.s_1 = 0;
        self.s_2 = 0;
        self.stop_after_block = false;
        self.endx_pending = false;
        self.decoded_block_count = 0;
        self.loop_addr_locked = false;
        self.last_sample = 0;
    }

    /// Trigger release phase on KOFF -- envelope drops toward zero at
    /// the configured release rate. Voice stays audible until envelope
    /// reaches 0, then moves to `Off`.
    fn key_off(&mut self) {
        if self.phase != AdsrPhase::Off {
            self.phase = AdsrPhase::Release;
        }
    }

    fn push_interpolation_sample(&mut self, sample: i16) {
        self.interp_ring[self.interp_pos] = sample;
        self.interp_pos = (self.interp_pos + 1) & 3;
    }

    fn interpolation_window(&self) -> [i16; 4] {
        [
            self.interp_ring[self.interp_pos],
            self.interp_ring[(self.interp_pos + 1) & 3],
            self.interp_ring[(self.interp_pos + 2) & 3],
            self.interp_ring[(self.interp_pos + 3) & 3],
        ]
    }

    /// Advance the ADSR envelope by one sample. Returns the current
    /// envelope level after the step (0..=0x7FFF, Q15).
    fn step_envelope(&mut self) -> i32 {
        match self.phase {
            // An inactive generator leaves a software-written ENVX value
            // latched. KON explicitly resets it to zero, and Release writes
            // zero when it transitions to Off. Resetting on every sample
            // erased manual negative values before the CPU could read them.
            AdsrPhase::Off => self.envelope,
            AdsrPhase::Attack => self.step_attack(),
            AdsrPhase::Decay => self.step_decay(),
            AdsrPhase::Sustain => self.step_sustain(),
            AdsrPhase::Release => self.step_release(),
        }
    }

    fn step_attack(&mut self) -> i32 {
        let mut rate = self.adsr.attack_rate;
        if self.adsr.attack_exp && self.envelope >= 0x6000 {
            rate = (rate + 8).min(127);
        }
        let denom = envelope_denominator(rate as usize);
        self.envelope_sub += 1;
        if self.envelope_sub >= denom {
            self.envelope_sub = 0;
            self.envelope += envelope_numerator_increase(rate as usize);
        }
        if self.envelope >= 0x7FFF {
            self.envelope = 0x7FFF;
            self.phase = AdsrPhase::Decay;
        }
        self.envelope
    }

    fn step_decay(&mut self) -> i32 {
        // Decay rate is 0..=15, scaled to a 7-bit rate by *4. Decay is
        // ALWAYS an exponential decrease on real hardware -- it is not
        // gated on the release-mode bit (which is an independent ADSR
        // field). PSX-SPX resets the decay envelope with
        // exponential=true unconditionally ( UpdateADSREnvelope),
        // PSX-SPX hard-codes EnvelopeMode::Exponential for Decay, and
        // PSX-SPX states "decay mode is always Exponential decrease, and
        // thus cannot be set".
        let rate = (self.adsr.decay_rate * 4).min(127);
        let denom = envelope_denominator(rate as usize);
        self.envelope_sub += 1;
        if self.envelope_sub >= denom {
            self.envelope_sub = 0;
            let dec = envelope_numerator_decrease(rate as usize);
            self.envelope += (dec * self.envelope) >> 15;
        }
        if self.envelope < 0 {
            self.envelope = 0;
        }
        // Sustain level target: (sustain_level + 1) * 0x800 -- but the
        // Redux check uses the high nibble of envelope directly, which
        // is simpler and matches hardware.
        if ((self.envelope >> 11) & 0xF) <= self.adsr.sustain_level {
            self.phase = AdsrPhase::Sustain;
        }
        self.envelope
    }

    fn step_sustain(&mut self) -> i32 {
        let mut rate = self.adsr.sustain_rate;
        if self.adsr.sustain_increase {
            // Rising sustain -- matches Attack structurally.
            if self.adsr.sustain_exp && self.envelope >= 0x6000 {
                rate = (rate + 8).min(127);
            }
            let denom = envelope_denominator(rate as usize);
            self.envelope_sub += 1;
            if self.envelope_sub >= denom {
                self.envelope_sub = 0;
                self.envelope += envelope_numerator_increase(rate as usize);
            }
            if self.envelope > 0x7FFF {
                self.envelope = 0x7FFF;
            }
        } else {
            // Falling sustain -- structurally like Release but without
            // the voice-off transition.
            let denom = envelope_denominator(rate as usize);
            self.envelope_sub += 1;
            if self.envelope_sub >= denom {
                self.envelope_sub = 0;
                if self.adsr.sustain_exp {
                    let dec = envelope_numerator_decrease(rate as usize);
                    self.envelope += (dec * self.envelope) >> 15;
                } else {
                    self.envelope += envelope_numerator_decrease(rate as usize);
                }
            }
            if self.envelope < 0 {
                self.envelope = 0;
            }
        }
        self.envelope
    }

    fn step_release(&mut self) -> i32 {
        // Release rate 0..=31 scales to 7-bit rate by *4.
        let rate = (self.adsr.release_rate * 4).min(127);
        let denom = envelope_denominator(rate as usize);
        self.envelope_sub += 1;
        if self.envelope_sub >= denom {
            self.envelope_sub = 0;
            if self.adsr.release_exp {
                let dec = envelope_numerator_decrease(rate as usize);
                self.envelope += (dec * self.envelope) >> 15;
            } else {
                self.envelope += envelope_numerator_decrease(rate as usize);
            }
        }
        if self.envelope <= 0 {
            self.envelope = 0;
            self.phase = AdsrPhase::Off;
        }
        self.envelope
    }
}

// ===============================================================
//  Reverb state.
// ===============================================================

mod reverb_reg {
    pub const FB_SRC_A: usize = 0;
    pub const FB_SRC_B: usize = 1;
    pub const IIR_ALPHA: usize = 2;
    pub const ACC_COEF_A: usize = 3;
    pub const ACC_COEF_B: usize = 4;
    pub const ACC_COEF_C: usize = 5;
    pub const ACC_COEF_D: usize = 6;
    pub const IIR_COEF: usize = 7;
    pub const FB_ALPHA: usize = 8;
    pub const FB_X: usize = 9;
    pub const IIR_DEST_A0: usize = 10;
    pub const IIR_DEST_A1: usize = 11;
    pub const ACC_SRC_A0: usize = 12;
    pub const ACC_SRC_A1: usize = 13;
    pub const ACC_SRC_B0: usize = 14;
    pub const ACC_SRC_B1: usize = 15;
    pub const IIR_SRC_A0: usize = 16;
    pub const IIR_SRC_A1: usize = 17;
    pub const IIR_DEST_B0: usize = 18;
    pub const IIR_DEST_B1: usize = 19;
    pub const ACC_SRC_C0: usize = 20;
    pub const ACC_SRC_C1: usize = 21;
    pub const ACC_SRC_D0: usize = 22;
    pub const ACC_SRC_D1: usize = 23;
    pub const IIR_SRC_B1: usize = 24;
    pub const IIR_SRC_B0: usize = 25;
    pub const MIX_DEST_A0: usize = 26;
    pub const MIX_DEST_A1: usize = 27;
    pub const MIX_DEST_B0: usize = 28;
    pub const MIX_DEST_B1: usize = 29;
    pub const IN_COEF_L: usize = 30;
    pub const IN_COEF_R: usize = 31;
}

/// Runtime state for the SPU reverb work area. The coefficient and
/// offset registers live in `Spu::reverb_cfg`; this only tracks the
/// moving cursor and 22.05 kHz wet-output interpolation history.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct ReverbState {
    /// Current reverb work-address in SPU RAM halfwords. The reverb
    /// buffer spans `reverb_base_halfword..=0x3ffff` and wraps there.
    curr_addr: u32,
    /// Previous and current wet samples at the 22.05 kHz reverb rate.
    last_l: i32,
    last_r: i32,
    wet_l: i32,
    wet_r: i32,
    /// The hardware reverb core advances at half the SPU sample rate.
    /// We process on every other 44.1 kHz tick and linearly hold the
    /// second sample.
    process_this_sample: bool,
}

impl ReverbState {
    fn new() -> Self {
        Self {
            process_this_sample: true,
            ..Self::default()
        }
    }

    fn reset_output(&mut self) {
        self.last_l = 0;
        self.last_r = 0;
        self.wet_l = 0;
        self.wet_r = 0;
    }
}

// ===============================================================
//  SPU top-level state.
// ===============================================================

/// Full SPU state. Owns SPU RAM, all 24 voices, the register bank,
/// and the output audio buffer.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Spu {
    /// 512 KiB SPU RAM as u16 (256K halfwords). ADPCM blocks + reverb
    /// work area + decoded-buffer captures live here. DMA channel 4
    /// writes streams of u16s via [`Spu::dma_write`]; software sees
    /// round-trip consistency through the TRANSFER_FIFO register.
    /// Boxed array is well past serde's built-in 32-element cap, so it
    /// round-trips through [`crate::serde_big_array::boxed_array`].
    #[serde(with = "crate::serde_big_array::boxed_array")]
    ram: Box<[u16; SPU_RAM_HALFWORDS]>,
    /// 24 voices.
    voices: [Voice; NUM_VOICES],

    /// SPU control register (0x1F80_1DAA). Bit 15 = SPU enable, bit 6 =
    /// IRQ enable, bits 5..4 = RAM transfer mode, bit 7 = reverb master.
    spucnt: u16,
    /// SPU status register (0x1F80_1DAE). Lower 6 bits mirror SPUCNT.
    /// Bit 6 = IRQ-triggered latch (cleared by SPUCNT write with bit 6 clear).
    spustat: u16,
    /// Applied and pending copies of SPUSTAT's low-six-bit SPUCNT mirror.
    /// Silicon updates this mirror on the next 44.1 kHz sample boundary,
    /// independently from the immediately-readable SPUCNT write latch.
    #[serde(default)]
    spustat_control: u16,
    #[serde(default)]
    spustat_control_pending: Option<(u16, u64)>,
    /// DMA request bits 7..9 are asserted only while channel 4 is actively
    /// transferring, not merely because SPUCNT selects a DMA mode.
    #[serde(default)]
    dma_active: bool,
    /// Most recent non-Stop RAM transfer mode. SCPH-9902 returns to Stop a
    /// little sooner after DMA Read than after the write/manual paths.
    #[serde(default)]
    last_active_transfer_mode: u8,
    /// Enable transfer/capture timing measured on the late PAL SCPH-9902.
    /// Earlier consoles expose the conventional capture-half polarity and
    /// accept ManualWrite FIFO contents as soon as the control latch changes.
    #[serde(default)]
    scph_9902_timing: bool,
    /// IRQ address (byte addr into SPU RAM). Written as halfword value
    /// which is scaled by 8.
    irq_addr: u32,
    /// Data transfer address (current write position in SPU RAM, bytes).
    transfer_addr: u32,
    /// Raw transfer-address register value (halfword / 8 address).
    /// Stored so reads round-trip the software-visible value.
    transfer_addr_raw: u16,
    /// Data transfer control (usually 0x0004). Stored for round-trip.
    transfer_ctrl: u16,
    /// 32-halfword hardware transfer FIFO. CPU writes may fill this while
    /// SPUCNT is stopped; switching to ManualWrite drains the queued words to
    /// SPU RAM. Keeping the queue in architectural state is required for
    /// save-state fidelity and for the public ps1-tests memory-transfer case.
    transfer_fifo: std::collections::VecDeque<u16>,

    /// Main output volume Left.
    main_vol_l: VolumeEnvelope,
    /// Main output volume Right.
    main_vol_r: VolumeEnvelope,
    /// Current-main-volume registers. PCSX-Redux stores these in its
    /// raw SPU regArea; main-volume writes do not update them.
    current_main_vol_l: u16,
    current_main_vol_r: u16,
    /// Reverb output volume Left.
    reverb_vol_l: VolumeEnvelope,
    /// Reverb output volume Right.
    reverb_vol_r: VolumeEnvelope,
    /// CD audio input volume Left.
    cd_vol_l: VolumeEnvelope,
    /// CD audio input volume Right.
    cd_vol_r: VolumeEnvelope,
    /// External audio input volume Left.
    ext_vol_l: VolumeEnvelope,
    /// External audio input volume Right.
    ext_vol_r: VolumeEnvelope,
    /// Raw reverb work-area start register. Reads must round-trip the
    /// CPU-visible word even when the effective mixer start is disabled.
    reverb_base_raw: u16,
    /// Reverb work-area start (byte address).
    reverb_base: u32,
    /// Reverb cursor / wet-output history.
    reverb: ReverbState,

    /// KON last-written register value -- echoed back on reads so the
    /// BIOS's round-trip verification sees consistency. Real hardware
    /// latches the write into the register and acts on it one sample
    /// later; we decouple into a separate pending bitmap below.
    kon_raw: u32,
    /// KON pending bitmap -- set by software writes, consumed by the
    /// sample tick (voices start on their next tick to match hardware's
    /// one-sample KON latency). Drained via `mem::take` each sample.
    kon_pending: u32,
    /// KOFF last-written register value.
    koff_raw: u32,
    /// KOFF pending bitmap.
    koff_pending: u32,
    /// Diagnostic per-voice activity: key-on count and number of samples
    /// that produced nonzero output. A voice keyed but never voiced is the
    /// signature of a missing-instrument bug (ADSR/decode failure). Not on
    /// any hot register path. Excluded from save states.
    #[serde(skip)]
    dbg_kon_count: [u32; NUM_VOICES],
    #[serde(skip)]
    dbg_voiced_samples: [u32; NUM_VOICES],
    /// Per-voice note-end reason tally: key-off (game gate) vs sample-stop
    /// (one-shot/loop end). Distinguishes a gate/release cutoff from samples
    /// ending early. Excluded from save states.
    #[serde(skip)]
    dbg_koff_count: [u32; NUM_VOICES],
    #[serde(skip)]
    dbg_sampstop_count: [u32; NUM_VOICES],
    /// Voice config captured at the last key-on: (start_addr, adsr_lo,
    /// adsr_hi, raw_pitch, vol_l, vol_r). Reveals why a keyed voice stays
    /// silent (bad start address, dead ADSR, zero volume). Excluded from
    /// save states.
    #[serde(skip)]
    dbg_keyon_cfg: [(u32, u16, u16, u16, i16, i16); NUM_VOICES],
    /// Per-voice envelope/decode trace, snapshotted every 1024 output
    /// samples: (decoded_sample_pre_adsr, envelope_level, phase). Bisects
    /// premature note cutoff -- decoded->0 with envelope held = decode/loop
    /// bug; envelope->0 = ADSR/key-off. Capped, diagnostic-only. Excluded
    /// from save states.
    #[serde(skip)]
    dbg_trace: [Vec<(i16, i32, u8)>; NUM_VOICES],
    /// Output-sample clock that gates dbg_trace snapshots. Excluded from
    /// save states.
    #[serde(skip)]
    dbg_sample_idx: u32,
    /// Per-voice running max of |decoded sample| and envelope within the
    /// current trace window; pushed + reset at each snapshot boundary.
    /// Excluded from save states.
    #[serde(skip)]
    dbg_acc_smax: [i32; NUM_VOICES],
    #[serde(skip)]
    dbg_acc_emax: [i32; NUM_VOICES],
    /// Accumulated |dry| and |wet (reverb)| output magnitude over the run.
    /// A near-zero wet/dry ratio means the reverb bus is not contributing --
    /// missing reverb tails read as dry, "cut-off" notes vs the oracle.
    /// Excluded from save states.
    #[serde(skip)]
    dbg_dry_energy: u64,
    #[serde(skip)]
    dbg_wet_energy: u64,
    /// OR-accumulated pmon / noise bitmaps over the whole run -- captures
    /// any voice ever pitch-modulated or noise-mode, not just the end state.
    /// Excluded from save states.
    #[serde(skip)]
    dbg_pmon_ever: u32,
    #[serde(skip)]
    dbg_noise_ever: u32,
    /// Voice pitch-modulation enable bitmap. Bit N means voice N takes
    /// its pitch from voice N-1's output sample.
    pmon: u32,
    /// Noise-mode enable bitmap. Bit N means voice N plays noise
    /// instead of its ADPCM sample.
    noise_on: u32,
    /// Reverb enable bitmap per voice.
    reverb_on: u32,
    /// ENDX latch -- each voice sets its bit when an ADPCM block with
    /// flag-1 (loop-end) was decoded. Software reads + write-1-to-clears.
    endx_latched: u32,

    /// Reverb configuration area (0x1F80_1DC0..=0x1F80_1DFE). Stored
    /// verbatim for round-trip reads. Mix path is not wired yet.
    reverb_cfg: [u16; 32],

    /// Host-facing stereo output buffer. Frontend pulls periodically via
    /// [`Spu::drain_audio`]. Oldest-sample-dropped when cap exceeded.
    /// Transient host-audio plumbing -- excluded from save states, reset
    /// empty on load (there is nothing to "resume" in an audio queue).
    #[serde(skip)]
    audio_out: std::collections::VecDeque<(i16, i16)>,

    /// CD audio input queue -- stereo samples fed by the CDROM
    /// controller during CD-DA or XA ADPCM playback. The SPU's
    /// `tick_sample` path drains one sample per output sample and
    /// mixes it via `CD_VOL_L/R` into the main output. When the
    /// queue is empty, CD contribution is zero. Bounded at
    /// ~0.5 s to prevent runaway growth during emulator fast-
    /// forward. This is pending emulated input rather than host
    /// output: consuming it can mutate SPU RAM/capture state, so it
    /// must round-trip through save states.
    cd_audio_in: std::collections::VecDeque<(i16, i16)>,

    /// Absolute cycle count at which we last produced an audio sample.
    /// Used to catch up when the scheduler delivers a burst of ticks.
    last_sample_cycle: u64,
    /// Total samples produced since reset -- diagnostic counter.
    /// Excluded from save states.
    #[serde(skip)]
    samples_produced: u64,
    /// Redux's decoded/capture-buffer IRQ cursor. When enabled, the
    /// first 0x1000 bytes of SPU RAM are treated as four 0x400-byte
    /// capture rings; the cursor advances by one halfword per output
    /// sample and can trigger SPU IRQs for games that synchronize audio
    /// streaming on that low-memory address range.
    decode_irq_cursor: u32,
    /// SPU capture-buffer write index (byte offset 0..=0x3FE, even).
    /// Per the PSX-SPX spec the SPU mirrors CD-L/R and Voice1/Voice3 into
    /// SPU RAM 0x000/0x400/0x800/0xC00 each 44.1 kHz sample, all sharing
    /// this 0x400-byte ring index. SPUSTAT bit 11 reports which half of
    /// the ring is currently being written.
    capture_buffer_pos: u16,
    /// SPU IRQ pending flag -- bus drains this to decide whether to
    /// raise `IrqSource::Spu`. Set when an enabled IRQ-addr match
    /// occurs on a voice's read pointer or the transfer-FIFO write.
    irq_pending: bool,

    /// Current noise-generator output sample. Updated on each SPU
    /// tick at a rate controlled by SPUCNT bits 8-13 (noise clock /
    /// shift). Voices with their NON_LO/HI bit set emit this value
    /// instead of their ADPCM sample.
    noise_val: i16,
    /// Sub-sample counter for the noise clock. The noise shift
    /// register advances every `2^shift` SPU samples scaled by
    /// the noise step field -- matching the hardware "noise rate"
    /// table. Simplified here to a cycle counter that rolls over
    /// based on SPUCNT bits 8-13.
    noise_counter: u32,
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

impl Spu {
    /// Freshly-reset SPU. RAM is zeroed, voices are silent, registers
    /// at hardware defaults (SPUCNT = 0, SPUSTAT = 0). Software's first
    /// job is to write SPUCNT with the enable bit set, then seed voice
    /// registers + sample RAM before key-on.
    pub fn new() -> Self {
        // SAFETY: zeroed Box<[u16; N]> requires a zeroed alloc.
        let ram = vec![0u16; SPU_RAM_HALFWORDS]
            .into_boxed_slice()
            .try_into()
            .expect("exact size");
        Self {
            ram,
            voices: std::array::from_fn(|_| Voice::default()),
            spucnt: 0,
            spustat: 0,
            spustat_control: 0,
            spustat_control_pending: None,
            dma_active: false,
            last_active_transfer_mode: 0,
            scph_9902_timing: false,
            irq_addr: 0,
            transfer_addr: 0,
            transfer_addr_raw: 0,
            transfer_ctrl: 0x0004,
            transfer_fifo: std::collections::VecDeque::with_capacity(32),
            main_vol_l: VolumeEnvelope::new(),
            main_vol_r: VolumeEnvelope::new(),
            current_main_vol_l: 0,
            current_main_vol_r: 0,
            reverb_vol_l: VolumeEnvelope::new(),
            reverb_vol_r: VolumeEnvelope::new(),
            cd_vol_l: VolumeEnvelope::new(),
            cd_vol_r: VolumeEnvelope::new(),
            ext_vol_l: VolumeEnvelope::new(),
            ext_vol_r: VolumeEnvelope::new(),
            reverb_base_raw: 0,
            reverb_base: 0,
            reverb: ReverbState::new(),
            kon_raw: 0,
            kon_pending: 0,
            koff_raw: 0,
            koff_pending: 0,
            dbg_kon_count: [0; NUM_VOICES],
            dbg_voiced_samples: [0; NUM_VOICES],
            dbg_koff_count: [0; NUM_VOICES],
            dbg_sampstop_count: [0; NUM_VOICES],
            dbg_keyon_cfg: [(0, 0, 0, 0, 0, 0); NUM_VOICES],
            dbg_trace: core::array::from_fn(|_| Vec::new()),
            dbg_sample_idx: 0,
            dbg_acc_smax: [0; NUM_VOICES],
            dbg_acc_emax: [0; NUM_VOICES],
            dbg_dry_energy: 0,
            dbg_wet_energy: 0,
            dbg_pmon_ever: 0,
            dbg_noise_ever: 0,
            pmon: 0,
            noise_on: 0,
            reverb_on: 0,
            endx_latched: 0,
            reverb_cfg: [0; 32],
            audio_out: std::collections::VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            cd_audio_in: std::collections::VecDeque::with_capacity(OUTPUT_BUFFER_CAP),
            last_sample_cycle: 0,
            samples_produced: 0,
            decode_irq_cursor: 0,
            capture_buffer_pos: 0,
            irq_pending: false,
            // Redux/hardware reset value -- must be non-zero or the
            // LFSR's NoiseWaveAdd lookup is stuck at index 0 forever.
            noise_val: 1,
            noise_counter: 0,
        }
    }

    /// Select the transfer/capture behavior measured on a late PAL PSone.
    pub fn apply_scph_9902_profile(&mut self) {
        self.scph_9902_timing = true;
        if self.capture_buffer_pos >= 0x200 {
            self.spustat |= 1 << 11;
        } else {
            self.spustat &= !(1 << 11);
        }
    }

    /// Apply the retail BIOS shell audio state inherited by a disc executable.
    ///
    /// A real-console PA5 capture established that the BIOS hands off with a
    /// fully configured reverb preset: all voices routed to EON, non-zero wet
    /// depth, and a work area at `0xE128 * 8`. Warm disc fast boot must retain
    /// this observable peripheral state even though it skips the license/shell
    /// path that normally programs it. Games remain responsible for resetting
    /// the SPU before loading their own banks.
    pub fn apply_retail_bios_shell_audio_profile(&mut self) {
        const BIOS_REVERB_CFG: [u16; 32] = [
            0x033D, 0x0231, 0x7E00, 0x5000, 0xB400, 0xB000, 0x4C00, 0xB000, 0x6000, 0x5400, 0x1ED6,
            0x1A31, 0x1D14, 0x183B, 0x1BC2, 0x16B2, 0x1A32, 0x15EF, 0x15EE, 0x1055, 0x1334, 0x0F2D,
            0x11F6, 0x0C5D, 0x1056, 0x0AE1, 0x0AE0, 0x07A2, 0x0464, 0x0232, 0x8000, 0x8000,
        ];

        self.write16(REVERB_VOL_L, 0x5EBC);
        self.write16(REVERB_VOL_R, 0x5EBC);
        self.write16(REVERB_BASE, 0xE128);
        self.write16(EON_LO, 0xFFFF);
        self.write16(EON_HI, 0x00FF);
        self.write16(EXT_VOL_L, 0);
        self.write16(EXT_VOL_R, 0);
        for (index, value) in BIOS_REVERB_CFG.into_iter().enumerate() {
            self.write16(REVERB_CFG_BASE + index as u32 * 2, value);
        }

        // PA5 sampled SPUCNT/SPUSTAT as C085/0805 before the SDK touched the
        // device. Apply the already-settled control mirror rather than leaving
        // a synthetic one-sample pending transition at the EXE entry point.
        self.spucnt = 0xC085;
        self.spustat_control = 0x0005;
        self.spustat_control_pending = None;
        self.spustat = (self.spustat & !0x083F) | 0x0800 | self.spustat_control;
        self.reverb.reset_output();
    }

    /// Advance the noise generator by one SPU sample. Port of
    /// PCSX-Redux's `NoiseClock` (Dr. Hell / Xebra algorithm), which
    /// in turn matches measurements from a real PSX SPU.
    ///
    /// SPUCNT bits 13:8 form a single 6-bit `noise_clock` field
    /// (high 4 bits = shift, low 2 bits = step). The threshold is
    /// `(0x8000 >> (clock >> 2)) << 16`. Each sample we add `0x10000`
    /// plus a fractional `NoiseFreqAdd[clock & 3]` to a 32-bit
    /// counter; whenever it crosses the threshold the LFSR shifts
    /// left and feeds in the new low bit from `NoiseWaveAdd[(val>>10) & 63]`.
    fn noise_tick(&mut self) {
        // Hardware "form" table -- bit pattern injected into the
        // LFSR low bit when it shifts.
        const NOISE_WAVE_ADD: [u8; 64] = [
            1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0,
            1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1,
            1, 0, 1, 0, 0, 1,
        ];
        // Hardware "fraction" table -- sub-sample increment per
        // step value (low 2 bits of clock); index 4 is the
        // wraparound threshold.
        const NOISE_FREQ_ADD: [u32; 5] = [0, 84, 140, 180, 210];

        let clock = ((self.spucnt >> 8) & 0x3F) as u32;
        let level = (0x8000u32 >> (clock >> 2)) << 16;

        self.noise_counter = self.noise_counter.wrapping_add(0x10000);

        let step_idx = (clock & 3) as usize;
        self.noise_counter = self.noise_counter.wrapping_add(NOISE_FREQ_ADD[step_idx]);
        if (self.noise_counter & 0xFFFF) >= NOISE_FREQ_ADD[4] {
            self.noise_counter = self.noise_counter.wrapping_add(0x10000);
            self.noise_counter = self.noise_counter.wrapping_sub(NOISE_FREQ_ADD[step_idx]);
        }

        if self.noise_counter >= level {
            while self.noise_counter >= level {
                self.noise_counter = self.noise_counter.wrapping_sub(level);
            }
            let v = self.noise_val as u16;
            let new_bit = NOISE_WAVE_ADD[((v as u32 >> 10) & 63) as usize] as u16;
            self.noise_val = ((v << 1) | new_bit) as i16;
        }
    }

    /// Enqueue a batch of stereo samples from the CDROM -- either
    /// CD-DA (Red Book) or decoded XA ADPCM. Consumed one sample
    /// per SPU output sample during `tick_sample`. Scaled by
    /// `CD_VOL_L/R` before mix. Caps at ~0.5 s of queued audio to
    /// keep memory bounded under fast-forward.
    pub fn feed_cd_audio(&mut self, samples: &[(i16, i16)]) {
        let cap = 22_050; // ~0.5 s at 44.1 kHz
        let overflow = (self.cd_audio_in.len() + samples.len()).saturating_sub(cap);
        for _ in 0..overflow {
            self.cd_audio_in.pop_front();
        }
        self.cd_audio_in.extend(samples.iter().copied());
    }

    /// Depth of the CD audio input queue. Diagnostic.
    pub fn cd_audio_queue_len(&self) -> usize {
        self.cd_audio_in.len()
    }

    /// Low edge of the SPU MMIO range.
    pub const BASE: u32 = SPU_BASE;
    /// High edge (exclusive). 0x200 bytes total.
    pub const END: u32 = SPU_END;

    /// `true` when `phys` falls inside the SPU register region.
    pub fn contains(phys: u32) -> bool {
        (Self::BASE..Self::END).contains(&phys)
    }

    /// Schedule the first SPU sample tick. Bus calls this once during
    /// construction -- subsequent reschedules happen inside the drain
    /// handler. We tick every [`SAMPLE_CYCLES`] cycles.
    pub fn seed_scheduler(scheduler: &mut Scheduler, now: u64) {
        scheduler.schedule(EventSlot::SpuAsync, now, SAMPLE_CYCLES);
    }

    /// Current SPUCNT value.
    pub fn spucnt(&self) -> u16 {
        self.spucnt
    }

    /// Current SPUSTAT value. Lower 6 bits mirror SPUCNT; bit 6 is the
    /// IRQ latch (set when an enabled SPU IRQ has fired, cleared by
    /// software writing SPUCNT with bit 6 clear). Bits 7..9 are the
    /// DMA-request bits synthesised from the SPUCNT transfer mode, and
    /// bit 11 (capture-buffer half) is held in `self.spustat`.
    pub fn spustat(&self) -> u16 {
        self.spustat_at(u64::MAX)
    }

    /// SPUSTAT sampled at a CPU cycle. The low control mirror crosses into
    /// the status domain at the next SPU sample edge.
    pub fn spustat_at(&self, now: u64) -> u16 {
        // Bits 0..5 mirror SPUCNT; bits 6/10/11 are held in self.spustat
        // (IRQ latch / transfer-busy / capture-buffer half).
        let control = match self.spustat_control_pending {
            Some((value, deadline)) if deadline <= now => value,
            _ => self.spustat_control,
        };
        let mut s = (self.spustat & !0x3F) | control;
        // DMA-request status bits (PSX-SPX SPUSTAT): bit 7 = DMA
        // read/write request (mirrors SPUCNT bit 5, i.e. set for both
        // DMA transfer modes), bit 8 = DMA write request (transfer
        // mode 2), bit 9 = DMA read request (transfer mode 3).
        match ((self.spucnt >> 4) & 3, self.dma_active) {
            (2, true) => s |= (1 << 7) | (1 << 8),
            (3, true) => s |= (1 << 7) | (1 << 9),
            _ => {}
        }
        s
    }

    /// Mark DMA channel 4 as owning the SPU transfer engine.
    pub fn begin_dma(&mut self, now: u64) {
        self.dma_active = true;
        // DMA-read mode's control mirror is gated by ownership of the
        // transfer engine on SCPH-9902. Once channel 4 is armed it crosses at
        // the normal sample boundary, just like the other control modes.
        if (self.spucnt >> 4) & 3 == 3 {
            let next_sample = now
                .saturating_div(SAMPLE_CYCLES)
                .saturating_add(1)
                .saturating_mul(SAMPLE_CYCLES);
            self.spustat_control_pending = Some((self.spucnt & 0x3F, next_sample));
        }
    }

    /// Release the SPU transfer engine when the scheduled DMA completes.
    pub fn end_dma(&mut self) {
        self.dma_active = false;
    }

    /// Diagnostic: total samples produced since reset. One sample pair
    /// per [`SAMPLE_CYCLES`] cycles.
    pub fn samples_produced(&self) -> u64 {
        self.samples_produced
    }

    /// Raw SPU RAM for deterministic headless diagnostics and save tooling.
    pub fn ram_halfwords(&self) -> &[u16] {
        &self.ram[..]
    }

    /// Diagnostic SPU IRQ state: (irq_addr, spucnt, spustat, decode cursor,
    /// cd-audio queue len). For tracing games that sync on the SPU IRQ.
    pub fn debug_irq_state(&self) -> (u32, u16, u16, u32, usize) {
        (
            self.irq_addr,
            self.spucnt,
            self.spustat,
            self.decode_irq_cursor,
            self.cd_audio_in.len(),
        )
    }

    /// Drain pending host-facing stereo samples. Frontend calls this
    /// every frame to feed its audio output. Returns `(left, right)`
    /// pairs in playback order, oldest first.
    pub fn drain_audio(&mut self) -> Vec<(i16, i16)> {
        self.audio_out.drain(..).collect()
    }

    /// How many stereo samples are queued but not yet drained.
    pub fn audio_queue_len(&self) -> usize {
        self.audio_out.len()
    }

    /// Diagnostic: per-voice (key-on count, samples that produced nonzero
    /// output). A voice with key-ons but ~no voiced samples is keyed but
    /// silent -- the signature of a missing-instrument bug.
    pub fn voice_debug_counts(&self) -> ([u32; NUM_VOICES], [u32; NUM_VOICES]) {
        (self.dbg_kon_count, self.dbg_voiced_samples)
    }

    /// Diagnostic: per-voice note-end tally (key-off count, sample-stop count).
    pub fn voice_end_counts(&self) -> ([u32; NUM_VOICES], [u32; NUM_VOICES]) {
        (self.dbg_koff_count, self.dbg_sampstop_count)
    }

    /// Diagnostic: per-voice (start_addr, adsr_lo, adsr_hi, raw_pitch,
    /// vol_l, vol_r) captured at the last key-on.
    pub fn voice_trace(&self) -> &[Vec<(i16, i32, u8)>; NUM_VOICES] {
        &self.dbg_trace
    }

    /// Diagnostic: (dry_energy, wet_energy, spucnt, reverb_on, pmon, noise_on)
    /// to gauge reverb activity + which voices are pitch-modulated / noise.
    pub fn reverb_debug(&self) -> (u64, u64, u16, u32, u32, u32) {
        (
            self.dbg_dry_energy,
            self.dbg_wet_energy,
            self.spucnt,
            self.reverb_on,
            self.dbg_pmon_ever,
            self.dbg_noise_ever,
        )
    }

    /// Diagnostic: per-voice (start_addr, adsr_lo, adsr_hi, raw_pitch,
    /// vol_l, vol_r) captured at the last key-on.
    pub fn voice_keyon_cfg(&self) -> [(u32, u16, u16, u16, i16, i16); NUM_VOICES] {
        self.dbg_keyon_cfg
    }

    /// True when the SPU has an IRQ to report. Bus drains this flag
    /// once per scheduler tick and raises `IrqSource::Spu` if so.
    pub fn take_irq_pending(&mut self) -> bool {
        std::mem::replace(&mut self.irq_pending, false)
    }

    /// True when SPU DMA (channel 4) is currently accepting transfers.
    /// Bus uses this to gate CHCR start-bit triggers on channel 4.
    pub fn dma_transfer_enabled(&self) -> bool {
        // SPUCNT bits 5..4 = 2 (DMA write) or 3 (DMA read) means RAM
        // transfer is DMA-driven.
        matches!((self.spucnt >> 4) & 3, 2 | 3)
    }

    /// Whether DMA-write mode has crossed into the SPU sample-clock domain.
    /// The SPUCNT latch changes immediately, but the transfer engine cannot
    /// accept RAM data until the corresponding SPUSTAT mirror is applied.
    pub fn dma_write_ready_at(&self, now: u64) -> bool {
        (self.spustat_at(now) >> 4) & 3 == 2
    }

    /// Diagnostic: current SPU-RAM transfer (write/read) cursor.
    pub fn transfer_addr(&self) -> u32 {
        self.transfer_addr
    }

    // ============================================================
    //  Register access -- byte / halfword / word, with cycle context.
    // ============================================================

    /// 8-bit read. Pulls the right byte out of the underlying 16-bit
    /// register.
    pub fn read8(&self, phys: u32) -> u8 {
        let word = self.read16_at(phys & !1, 0);
        if phys & 1 == 0 {
            word as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// 8-bit write. Merges into the existing 16-bit register.
    pub fn write8(&mut self, phys: u32, value: u8) {
        let aligned = phys & !1;
        let word = self.read16_at(aligned, 0);
        let merged = if phys & 1 == 0 {
            (word & 0xFF00) | value as u16
        } else {
            (word & 0x00FF) | ((value as u16) << 8)
        };
        self.write16_at(aligned, merged, 0);
    }

    /// 32-bit read. Splits into two halfword reads; upper half is 0 on
    /// real hardware for most registers.
    pub fn read32(&self, phys: u32) -> u32 {
        self.read32_at(phys, 0)
    }

    /// 32-bit read with cycle context.
    pub fn read32_at(&self, phys: u32, now: u64) -> u32 {
        let lo = self.read16_at(phys, now) as u32;
        let hi = self.read16_at(phys.wrapping_add(2), now) as u32;
        lo | (hi << 16)
    }

    /// 16-bit read (no cycle context -- used by legacy callers; all
    /// registers we care about are cycle-independent).
    pub fn read16(&self, phys: u32) -> u16 {
        self.read16_at(phys, 0)
    }

    /// 16-bit read with cycle context.
    pub fn read16_at(&self, phys: u32, now: u64) -> u16 {
        let phys = phys & !1;
        if let Some((v, off)) = decode_voice(phys) {
            return self.read_voice_reg(v, off);
        }
        match phys {
            MAIN_VOL_L => self.main_vol_l.reg_read(),
            MAIN_VOL_R => self.main_vol_r.reg_read(),
            CURRENT_MAIN_VOL_L => self.current_main_vol_l,
            CURRENT_MAIN_VOL_R => self.current_main_vol_r,
            REVERB_VOL_L => self.reverb_vol_l.reg_read(),
            REVERB_VOL_R => self.reverb_vol_r.reg_read(),
            KON_LO => self.kon_raw as u16,
            KON_HI => (self.kon_raw >> 16) as u16,
            KOFF_LO => self.koff_raw as u16,
            KOFF_HI => (self.koff_raw >> 16) as u16,
            PMON_LO => self.pmon as u16,
            PMON_HI => (self.pmon >> 16) as u16,
            NON_LO => self.noise_on as u16,
            NON_HI => (self.noise_on >> 16) as u16,
            EON_LO => self.reverb_on as u16,
            EON_HI => (self.reverb_on >> 16) as u16,
            ENDX_LO => self.endx_latched as u16,
            ENDX_HI => (self.endx_latched >> 16) as u16,
            REVERB_BASE => self.reverb_base_raw,
            IRQ_ADDR => (self.irq_addr >> 3) as u16,
            TRANSFER_ADDR => self.transfer_addr_raw,
            TRANSFER_FIFO => self.transfer_fifo_read(),
            SPUCNT => self.spucnt,
            TRANSFER_CTRL => self.transfer_ctrl,
            SPUSTAT => self.spustat_at(now),
            CD_VOL_L => self.cd_vol_l.reg_read(),
            CD_VOL_R => self.cd_vol_r.reg_read(),
            EXT_VOL_L => self.ext_vol_l.reg_read(),
            EXT_VOL_R => self.ext_vol_r.reg_read(),
            a if (REVERB_CFG_BASE..REVERB_CFG_BASE + 64).contains(&a) => {
                let idx = ((a - REVERB_CFG_BASE) >> 1) as usize;
                self.reverb_cfg[idx]
            }
            _ => 0,
        }
    }

    /// 16-bit write.
    pub fn write16(&mut self, phys: u32, value: u16) {
        self.write16_at(phys, value, 0);
    }

    /// 16-bit write with cycle context.
    pub fn write16_at(&mut self, phys: u32, value: u16, now: u64) {
        let phys = phys & !1;
        if let Some((v, off)) = decode_voice(phys) {
            self.write_voice_reg(v, off, value);
            return;
        }
        match phys {
            MAIN_VOL_L => self.main_vol_l.write(value),
            MAIN_VOL_R => self.main_vol_r.write(value),
            CURRENT_MAIN_VOL_L => self.current_main_vol_l = value,
            CURRENT_MAIN_VOL_R => self.current_main_vol_r = value,
            REVERB_VOL_L => self.reverb_vol_l.write_signed_q15(value),
            REVERB_VOL_R => self.reverb_vol_r.write_signed_q15(value),
            KON_LO => self.queue_kon(value, 0),
            KON_HI => self.queue_kon(value, 16),
            KOFF_LO => self.queue_koff(value, 0),
            KOFF_HI => self.queue_koff(value, 16),
            PMON_LO => self.pmon = (self.pmon & 0xFFFF_0000) | value as u32,
            PMON_HI => self.pmon = (self.pmon & 0x0000_FFFF) | ((value as u32) << 16),
            NON_LO => self.noise_on = (self.noise_on & 0xFFFF_0000) | value as u32,
            NON_HI => self.noise_on = (self.noise_on & 0x0000_FFFF) | ((value as u32) << 16),
            EON_LO => self.reverb_on = (self.reverb_on & 0xFFFF_0000) | value as u32,
            EON_HI => self.reverb_on = (self.reverb_on & 0x0000_FFFF) | ((value as u32) << 16),
            ENDX_LO => self.endx_latched &= !(value as u32),
            ENDX_HI => self.endx_latched &= !((value as u32) << 16),
            REVERB_BASE => self.write_reverb_base(value),
            IRQ_ADDR => self.irq_addr = (value as u32) << 3,
            TRANSFER_ADDR => {
                self.transfer_addr_raw = value;
                self.transfer_addr = (value as u32) << 3;
            }
            TRANSFER_FIFO => self.transfer_fifo_write(value, now),
            SPUCNT => self.write_spucnt(value, now),
            TRANSFER_CTRL => self.transfer_ctrl = value,
            SPUSTAT => { /* read-only -- writes dropped */ }
            CD_VOL_L => self.cd_vol_l.write_signed_q15(value),
            CD_VOL_R => self.cd_vol_r.write_signed_q15(value),
            EXT_VOL_L => self.ext_vol_l.write_signed_q15(value),
            EXT_VOL_R => self.ext_vol_r.write_signed_q15(value),
            a if (REVERB_CFG_BASE..REVERB_CFG_BASE + 64).contains(&a) => {
                let idx = ((a - REVERB_CFG_BASE) >> 1) as usize;
                self.reverb_cfg[idx] = value;
            }
            _ => {}
        }
    }

    /// 32-bit write -- splits into two halfword writes.
    pub fn write32(&mut self, phys: u32, value: u32) {
        self.write32_at(phys, value, 0);
    }

    /// 32-bit write with cycle context.
    pub fn write32_at(&mut self, phys: u32, value: u32, now: u64) {
        self.write16_at(phys, value as u16, now);
        self.write16_at(phys.wrapping_add(2), (value >> 16) as u16, now);
    }

    fn write_reverb_base(&mut self, value: u16) {
        self.reverb_base_raw = value;
        // Redux treats the decode/capture-buffer region and 0xffff as
        // "reverb off"; keep the register readable, but disable the
        // effective work area so games don't accidentally smear over
        // low SPU RAM when they are just clearing the mixer.
        if value == 0xFFFF || value <= 0x0200 {
            self.reverb_base = 0;
            self.reverb.curr_addr = 0;
            self.reverb.reset_output();
            return;
        }

        let byte_addr = ((value as u32) << 3) & (SPU_RAM_BYTES as u32 - 1);
        if self.reverb_base != byte_addr {
            self.reverb_base = byte_addr;
            self.reverb.curr_addr = byte_addr >> 1;
            self.reverb.reset_output();
        }
    }

    fn write_spucnt(&mut self, value: u16, now: u64) {
        let prev = self.spucnt;
        if let Some((pending, deadline)) = self.spustat_control_pending {
            if deadline <= now {
                self.spustat_control = pending;
                self.spustat_control_pending = None;
            }
        }
        self.spucnt = value;
        let transfer_mode = (value >> 4) & 3;
        if self.scph_9902_timing {
            // All transfer modes cross into SPUSTAT at the next sample
            // boundary (SB4 2026-08-07: the mode-3 mirror settles in 24-27
            // polls with DMA verifiably not yet armed); Stop keeps its
            // measured asynchronous settle.
            if transfer_mode != 0 {
                self.last_active_transfer_mode = transfer_mode as u8;
            }
            let next_sample = if transfer_mode == 0 {
                let settle_cycles = if self.last_active_transfer_mode == 3 {
                    832
                } else {
                    928
                };
                now.saturating_add(settle_cycles)
            } else {
                now.saturating_div(SAMPLE_CYCLES)
                    .saturating_add(1)
                    .saturating_mul(SAMPLE_CYCLES)
            };
            self.spustat_control_pending = Some((value & 0x3F, next_sample));
        } else {
            let next_sample = now
                .saturating_div(SAMPLE_CYCLES)
                .saturating_add(1)
                .saturating_mul(SAMPLE_CYCLES);
            self.spustat_control_pending = Some((value & 0x3F, next_sample));
            if transfer_mode == 1 {
                self.drain_transfer_fifo_to_ram();
            }
        }
        // SPU IRQ enable transitioned to 0 → clear status latch (ack).
        if (prev & (1 << 6)) != 0 && (value & (1 << 6)) == 0 {
            self.spustat &= !(1 << 6);
        }
    }

    fn read_voice_reg(&self, v: usize, off: u32) -> u16 {
        let voice = &self.voices[v];
        match off {
            voice_offset::VOLUME_L => voice.vol_l.reg_read(),
            voice_offset::VOLUME_R => voice.vol_r.reg_read(),
            voice_offset::PITCH => voice.raw_pitch,
            voice_offset::START_ADDR => voice.start_addr_raw,
            voice_offset::ADSR_LO => voice.adsr_lo,
            voice_offset::ADSR_HI => voice.adsr_hi,
            voice_offset::ADSR_CURRENT => {
                // Current ADSR volume (ENVX), signed 16-bit when written by
                // software. The automatic generator normally occupies
                // 0..=7FFF, but SCPH-9902 reads back manual FFFF writes
                // verbatim, matching PSX-SPX's documented -8000..+7FFF
                // manual range. Games also poll the live generated value:
                // Some titles wait for every voice to reach zero.
                //
                // This deliberately diverges from the Redux parity trace --
                // Redux's SPU runs on an unpumped background thread during an
                // oracle trace, so its `readRegister` case 12 returns a stale
                // 1 -- but per the hardware > Redux oracle priority the live
                // envelope is the correct, hardware-accurate value.
                self.voices[v]
                    .envelope
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16
            }
            voice_offset::REPEAT_ADDR => voice.loop_addr_raw,
            _ => 0,
        }
    }

    fn write_voice_reg(&mut self, v: usize, off: u32, value: u16) {
        let voice = &mut self.voices[v];
        match off {
            voice_offset::VOLUME_L => voice.vol_l.write(value),
            voice_offset::VOLUME_R => voice.vol_r.write(value),
            voice_offset::PITCH => voice.raw_pitch = value,
            voice_offset::START_ADDR => {
                // Echo the full 16-bit register on read; the decoder uses
                // the <<3, 16-byte-aligned byte address.
                voice.start_addr_raw = value;
                voice.start_addr = ((value as u32) << 3) & (SPU_RAM_BYTES as u32 - 1) & !0xF;
            }
            voice_offset::ADSR_LO => {
                voice.adsr_lo = value;
                parse_adsr_lo(value, &mut voice.adsr);
            }
            voice_offset::ADSR_HI => {
                voice.adsr_hi = value;
                parse_adsr_hi(value, &mut voice.adsr);
            }
            voice_offset::ADSR_CURRENT => {
                // Manual writes are signed 16-bit. The ADSR generator will
                // overwrite/step this value on its next active envelope tick.
                voice.envelope = value as i16 as i32;
            }
            voice_offset::REPEAT_ADDR => {
                // A software REPEAT_ADDR write stores the loop address and
                // normally locks it (suppresses the ADPCM loop-start flag's
                // auto-update). But while the voice is ON and still in its
                // very first ADPCM block, hardware lets the sample's own
                // loop-start flag override the written value, so we do NOT
                // lock in that window (PSX-SPX: `ignore =
                // !is_on || !first-block`, OR-ed into the loop-address lock;
                // Tron Bonne / Valkyrie Profile / Re-Loaded depend on this).
                // `decoded_block_count <= 1` is that first-block window
                // (0 = before the first decode, 1 = within the first block).
                voice.loop_addr_raw = value;
                voice.loop_addr = ((value as u32) << 3) & (SPU_RAM_BYTES as u32 - 1) & !0xF;
                let ignore = voice.phase == AdsrPhase::Off || voice.decoded_block_count > 1;
                voice.loop_addr_locked |= ignore;
            }
            _ => {}
        }
    }

    fn queue_kon(&mut self, mask: u16, shift: u32) {
        let bits = (mask as u32) << shift;
        // Raw register -- reads echo this back verbatim. Whole-half
        // overwrite semantics: writing KON_LO replaces the low 16 bits,
        // KON_HI replaces the high 16 bits.
        let clear_mask = !(0xFFFFu32 << shift);
        self.kon_raw = (self.kon_raw & clear_mask) | bits;
        // Pending bitmap -- OR-accumulates so multiple writes before a
        // sample tick all fire.
        self.kon_pending |= bits;
        // A fresh KON clears the ENDX bits for those voices.
        self.endx_latched &= !bits;
    }

    fn queue_koff(&mut self, mask: u16, shift: u32) {
        let bits = (mask as u32) << shift;
        let clear_mask = !(0xFFFFu32 << shift);
        self.koff_raw = (self.koff_raw & clear_mask) | bits;
        self.koff_pending |= bits;
    }

    // ============================================================
    //  Data transfer FIFO -- software-driven SPU RAM access.
    // ============================================================

    fn transfer_fifo_write(&mut self, value: u16, now: u64) {
        const FIFO_HALFWORDS: usize = 32;
        if self.transfer_fifo.len() == FIFO_HALFWORDS {
            // A full hardware FIFO cannot accept another halfword. CPU-side
            // bus stalling is not yet represented, so preserve the existing
            // contents and ignore the overflow instead of corrupting order.
            return;
        }
        self.transfer_fifo.push_back(value);
        let manual_write_active = if self.scph_9902_timing {
            (self.spustat_at(now) >> 4) & 0x3 == 1
        } else {
            (self.spucnt >> 4) & 0x3 == 1
        };
        if manual_write_active {
            self.drain_transfer_fifo_to_ram();
        }
    }

    fn drain_transfer_fifo_to_ram(&mut self) {
        while let Some(value) = self.transfer_fifo.pop_front() {
            let idx = (self.transfer_addr >> 1) as usize % SPU_RAM_HALFWORDS;
            self.ram[idx] = value;
            self.check_irq_on_transfer();
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_BYTES as u32 - 1);
        }
        self.transfer_addr_raw = (self.transfer_addr >> 3) as u16;
    }

    fn transfer_fifo_read(&self) -> u16 {
        // Real hardware post-increments the transfer address on reads
        // too -- but reads can't come from `&self`. We return the value
        // at the current address and let a caller (`peek_transfer_fifo`)
        // do the increment if they want. For a const-read, this is
        // enough: writes are the common case.
        let idx = (self.transfer_addr >> 1) as usize % SPU_RAM_HALFWORDS;
        self.ram[idx]
    }

    /// An SPU RAM IRQ may only latch when IRQ9 is enabled (SPUCNT bit 6)
    /// **and** the sticky IRQ9 flag (SPUSTAT bit 6) is not already set.
    /// Once latched, no further IRQ can fire until software acknowledges
    /// by clearing SPUCNT bit 6 (which clears the SPUSTAT flag in
    /// `write_spucnt`). Mirrors PSX-SPX `is_irq_triggerable`
    /// (`irq9_enable && !irq9_flag`) and PSX-SPX `IsRAMIRQTriggerable`
    /// (`SPUCNT.irq9_enable && !SPUSTAT.irq9_flag`); nocash PSX-SPX: SPUSTAT.6
    /// is a sticky flag that blocks re-latching until acknowledged.
    fn irq_triggerable(&self) -> bool {
        (self.spucnt & (1 << 6)) != 0 && (self.spustat & (1 << 6)) == 0
    }

    fn check_irq_on_transfer(&mut self) {
        if !self.irq_triggerable() {
            return;
        }
        // IRQ fires when the transfer pointer reaches the IRQ address
        // (within a 2-byte window -- IRQ_ADDR granularity is 8 bytes
        // after the <<3 decode, so any write into that 8-byte range
        // triggers).
        let irq = self.irq_addr & !0x7;
        let cur = self.transfer_addr & !0x7;
        if irq == cur {
            self.spustat |= 1 << 6;
            self.irq_pending = true;
        }
    }

    /// Write one SPU capture-buffer halfword. The four capture banks live
    /// at SPU RAM 0x000 (CD-L), 0x400 (CD-R), 0x800 (Voice1) and 0xC00
    /// (Voice3); each is a 0x400-byte ring written at the shared
    /// `capture_buffer_pos`. Per the PSX-SPX spec a capture write can also
    /// latch an SPU IRQ when the armed IRQ address falls on the written
    /// halfword (same sticky IRQ9 gate as every other SPU-RAM access).
    fn write_to_capture(&mut self, bank: u32, value: u16) {
        let byte_addr = bank * 0x400 + self.capture_buffer_pos as u32;
        self.ram[(byte_addr >> 1) as usize] = value;
        if self.irq_triggerable() && (self.irq_addr & !0x7) == (byte_addr & !0x7) {
            self.spustat |= 1 << 6;
            self.irq_pending = true;
        }
    }

    // ============================================================
    //  DMA -- SPU channel 4.
    // ============================================================

    /// Stream halfwords from main RAM into SPU RAM at the current
    /// transfer address. Called by the bus when DMA channel 4 triggers
    /// a RAM→SPU transfer. `words` is a slice of halfwords to copy.
    pub fn dma_write(&mut self, words: &[u16]) {
        for &w in words {
            let idx = (self.transfer_addr >> 1) as usize % SPU_RAM_HALFWORDS;
            self.ram[idx] = w;
            self.check_irq_on_transfer();
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_BYTES as u32 - 1);
        }
        self.transfer_addr_raw = (self.transfer_addr >> 3) as u16;
    }

    /// Stream halfwords from SPU RAM back to main RAM at the current
    /// transfer address. Called on SPU→RAM DMA (rare; some games use
    /// it for live audio capture).
    pub fn dma_read(&mut self, words: &mut [u16]) {
        for w in words {
            let idx = (self.transfer_addr >> 1) as usize % SPU_RAM_HALFWORDS;
            *w = self.ram[idx];
            self.check_irq_on_transfer();
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_BYTES as u32 - 1);
        }
        self.transfer_addr_raw = (self.transfer_addr >> 3) as u16;
    }

    /// Stream SPU RAM through the block-shaped read FIFO. When the memory
    /// controller's DMA timing override is disabled, silicon inserts `FFFF`
    /// as the first halfword of every block and drops the block's last source
    /// halfword. This is the documented unstable SPU-read mode; callers that
    /// set bits 24..27 of the SPU delay register use the ordinary stable path.
    pub fn dma_read_blocks(&mut self, words: &mut [u16], block_halfwords: usize, stable: bool) {
        if stable || block_halfwords == 0 {
            self.dma_read(words);
            return;
        }

        for block in words.chunks_mut(block_halfwords) {
            if block.is_empty() {
                continue;
            }
            block[0] = 0xFFFF;
            for output in &mut block[1..] {
                let idx = (self.transfer_addr >> 1) as usize % SPU_RAM_HALFWORDS;
                *output = self.ram[idx];
                self.check_irq_on_transfer();
                self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_BYTES as u32 - 1);
            }
            // The FIFO-boundary insertion consumes the final source slot even
            // though that halfword is not delivered to main RAM.
            self.check_irq_on_transfer();
            self.transfer_addr = (self.transfer_addr + 2) & (SPU_RAM_BYTES as u32 - 1);
        }
        self.transfer_addr_raw = (self.transfer_addr >> 3) as u16;
    }

    // ============================================================
    //  Reverb -- Neill/Redux work-area network.
    // ============================================================

    fn reverb_base_halfword(&self) -> u32 {
        self.reverb_base >> 1
    }

    fn reverb_active(&self) -> bool {
        self.reverb_base != 0 && self.reverb_base < SPU_RAM_BYTES as u32
    }

    fn reverb_cfg_s(&self, idx: usize) -> i32 {
        self.reverb_cfg[idx] as i16 as i32
    }

    fn reverb_cfg_u(&self, idx: usize) -> i32 {
        self.reverb_cfg[idx] as i32
    }

    fn reverb_ram_index(&self, offset: i32, extra_halfwords: i32) -> usize {
        let start = self.reverb_base_halfword() as i32;
        if start >= SPU_RAM_HALFWORDS as i32 {
            return 0;
        }

        // Redux wraps the reverb work address with two while-loops, not
        // a clean modulo. The below-start case is subtly off by one
        // (`0x3ffff - delta`), so keep that quirk for parity.
        let mut idx = self.reverb.curr_addr as i32 + offset.saturating_mul(4) + extra_halfwords;
        while idx > 0x3FFFF {
            idx = start + (idx - 0x40000);
        }
        while idx < start {
            idx = 0x3FFFF - (start - idx);
        }
        idx.clamp(0, SPU_RAM_HALFWORDS as i32 - 1) as usize
    }

    fn reverb_read(&self, offset: i32) -> i32 {
        self.ram[self.reverb_ram_index(offset, 0)] as i16 as i32
    }

    /// Reverb read with an extra signed halfword offset, mirroring
    /// the hardware `ReverbRead(address, offset)`. The IIR_DEST same/
    /// different-side reflection taps read one cell *behind* the write
    /// target (`[mLSAME-2]` in nocash = -1 halfword).
    fn reverb_read_at(&self, offset: i32, extra_halfwords: i32) -> i32 {
        self.ram[self.reverb_ram_index(offset, extra_halfwords)] as i16 as i32
    }

    fn reverb_write(&mut self, offset: i32, extra_halfwords: i32, value: i32) {
        let idx = self.reverb_ram_index(offset, extra_halfwords);
        self.ram[idx] = saturate_i16(value) as u16;
    }

    fn mul_q15(a: i32, b: i32) -> i32 {
        ((a as i64 * b as i64) / 32768).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    fn scale_reverb_output(sample: i32, vol: i16) -> i32 {
        ((sample as i64 * vol as i64) / 0x4000).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    fn mix_reverb(&mut self, input_l: i32, input_r: i32) -> (i32, i32) {
        if !self.reverb_active() {
            self.reverb.reset_output();
            return (0, 0);
        }

        let processed_this_sample = self.reverb.process_this_sample;
        if processed_this_sample {
            // PA5 on real SCPH hardware proved that clearing the reverb-master
            // bit disables only the IIR/MIX_DEST writes. The read/APF/output
            // side keeps traversing SPU RAM: a later DMA into the BIOS reverb
            // work area immediately changed the audible wet output even with
            // SPUCNT=0xC000 and EON=0. Always evaluate the network here and
            // gate its RAM writes inside `run_reverb_step`.
            self.run_reverb_step(input_l, input_r);

            let mut next_addr = self.reverb.curr_addr.saturating_add(1);
            if next_addr >= SPU_RAM_HALFWORDS as u32 || next_addr < self.reverb_base_halfword() {
                next_addr = self.reverb_base_halfword();
            }
            self.reverb.curr_addr = next_addr;
        }

        let out = if processed_this_sample {
            let l = self.reverb.last_l + (self.reverb.wet_l - self.reverb.last_l) / 2;
            let r = self.reverb.last_r + (self.reverb.wet_r - self.reverb.last_r) / 2;
            // Redux's right-channel helper promotes iLastRVBRight to
            // iRVBRight after returning the interpolated sample. The
            // left helper does not do this, so the held sample on the
            // next 44.1 kHz tick is asymmetric: previous-left/current-right.
            self.reverb.last_r = self.reverb.wet_r;
            (l, r)
        } else {
            (self.reverb.last_l, self.reverb.wet_r)
        };
        self.reverb.process_this_sample = !self.reverb.process_this_sample;
        out
    }

    fn run_reverb_step(&mut self, input_l: i32, input_r: i32) {
        use reverb_reg::*;

        let writes_enabled = self.spucnt & SPUCNT_REVERB_MASTER_ENABLE != 0;

        let iir_coef = self.reverb_cfg_s(IIR_COEF);
        let iir_alpha = self.reverb_cfg_s(IIR_ALPHA);
        let in_coef_l = self.reverb_cfg_s(IN_COEF_L);
        let in_coef_r = self.reverb_cfg_s(IN_COEF_R);

        let iir_input_a0 = Self::mul_q15(self.reverb_read(self.reverb_cfg_s(IIR_SRC_A0)), iir_coef)
            + Self::mul_q15(input_l, in_coef_l);
        let iir_input_a1 = Self::mul_q15(self.reverb_read(self.reverb_cfg_s(IIR_SRC_A1)), iir_coef)
            + Self::mul_q15(input_r, in_coef_r);
        let iir_input_b0 = Self::mul_q15(self.reverb_read(self.reverb_cfg_s(IIR_SRC_B0)), iir_coef)
            + Self::mul_q15(input_l, in_coef_l);
        let iir_input_b1 = Self::mul_q15(self.reverb_read(self.reverb_cfg_s(IIR_SRC_B1)), iir_coef)
            + Self::mul_q15(input_r, in_coef_r);

        let inv_iir_alpha = 32768 - iir_alpha;
        let iir_a0 = Self::mul_q15(iir_input_a0, iir_alpha)
            + Self::mul_q15(
                self.reverb_read_at(self.reverb_cfg_s(IIR_DEST_A0), -1),
                inv_iir_alpha,
            );
        let iir_a1 = Self::mul_q15(iir_input_a1, iir_alpha)
            + Self::mul_q15(
                self.reverb_read_at(self.reverb_cfg_s(IIR_DEST_A1), -1),
                inv_iir_alpha,
            );
        let iir_b0 = Self::mul_q15(iir_input_b0, iir_alpha)
            + Self::mul_q15(
                self.reverb_read_at(self.reverb_cfg_s(IIR_DEST_B0), -1),
                inv_iir_alpha,
            );
        let iir_b1 = Self::mul_q15(iir_input_b1, iir_alpha)
            + Self::mul_q15(
                self.reverb_read_at(self.reverb_cfg_s(IIR_DEST_B1), -1),
                inv_iir_alpha,
            );

        if writes_enabled {
            self.reverb_write(self.reverb_cfg_s(IIR_DEST_A0), 0, iir_a0);
            self.reverb_write(self.reverb_cfg_s(IIR_DEST_A1), 0, iir_a1);
            self.reverb_write(self.reverb_cfg_s(IIR_DEST_B0), 0, iir_b0);
            self.reverb_write(self.reverb_cfg_s(IIR_DEST_B1), 0, iir_b1);
        }

        let acc0 = Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_A0)),
            self.reverb_cfg_s(ACC_COEF_A),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_B0)),
            self.reverb_cfg_s(ACC_COEF_B),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_C0)),
            self.reverb_cfg_s(ACC_COEF_C),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_D0)),
            self.reverb_cfg_s(ACC_COEF_D),
        );
        let acc1 = Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_A1)),
            self.reverb_cfg_s(ACC_COEF_A),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_B1)),
            self.reverb_cfg_s(ACC_COEF_B),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_C1)),
            self.reverb_cfg_s(ACC_COEF_C),
        ) + Self::mul_q15(
            self.reverb_read(self.reverb_cfg_s(ACC_SRC_D1)),
            self.reverb_cfg_s(ACC_COEF_D),
        );

        let fb_src_a = self.reverb_cfg_u(FB_SRC_A);
        let fb_src_b = self.reverb_cfg_s(FB_SRC_B);
        let mix_dest_a0 = self.reverb_cfg_s(MIX_DEST_A0);
        let mix_dest_a1 = self.reverb_cfg_s(MIX_DEST_A1);
        let mix_dest_b0 = self.reverb_cfg_s(MIX_DEST_B0);
        let mix_dest_b1 = self.reverb_cfg_s(MIX_DEST_B1);
        let fb_a0 = self.reverb_read(mix_dest_a0 - fb_src_a);
        let fb_a1 = self.reverb_read(mix_dest_a1 - fb_src_a);
        let fb_b0 = self.reverb_read(mix_dest_b0 - fb_src_b);
        let fb_b1 = self.reverb_read(mix_dest_b1 - fb_src_b);
        let fb_alpha = self.reverb_cfg_s(FB_ALPHA);
        let fb_x = self.reverb_cfg_s(FB_X);

        // Late Reverb APF1 (All-Pass Filter 1, input = comb-filter ACC).
        // MIX_DEST_A is the APF1 buffer, FB_SRC_A its delay tap:
        //   [mLAPF1] = ACC - vAPF1*[mLAPF1-dAPF1]
        //   Lout     = [mLAPF1]*vAPF1 + [mLAPF1-dAPF1]
        // reverb_write clamps to i16 when storing; the carried value stays
        // unclamped, exactly like PSX-SPX calculate_*_reverb (whose
        // apply_volume = `>>15`, the same convention as mul_q15).
        let mda0 = acc0 - Self::mul_q15(fb_a0, fb_alpha);
        let mda1 = acc1 - Self::mul_q15(fb_a1, fb_alpha);
        if writes_enabled {
            self.reverb_write(mix_dest_a0, 0, mda0);
            self.reverb_write(mix_dest_a1, 0, mda1);
        }
        let apf1_l = Self::mul_q15(mda0, fb_alpha) + fb_a0;
        let apf1_r = Self::mul_q15(mda1, fb_alpha) + fb_a1;

        // Late Reverb APF2 (All-Pass Filter 2, input = APF1 output).
        // MIX_DEST_B is the APF2 buffer, FB_SRC_B its delay tap:
        //   [mLAPF2] = APF1 - vAPF2*[mLAPF2-dAPF2]
        //   Lout     = [mLAPF2]*vAPF2 + [mLAPF2-dAPF2]
        let mdb0 = apf1_l - Self::mul_q15(fb_b0, fb_x);
        let mdb1 = apf1_r - Self::mul_q15(fb_b1, fb_x);
        if writes_enabled {
            self.reverb_write(mix_dest_b0, 0, mdb0);
            self.reverb_write(mix_dest_b1, 0, mdb1);
        }

        self.reverb.last_l = self.reverb.wet_l;
        self.reverb.last_r = self.reverb.wet_r;
        // Wet output is the APF2 result (`LeftOutput = Lout*vLOUT`, nocash SPU
        // Reverb Formula), NOT the old invented (MIX_DEST_A + MIX_DEST_B)/3.
        // Matches PSX-SPX `Clamp16(FB_B + ((MDB*FB_X)>>15))` and PSX-SPX
        // self.left_out/right_out (the post-APF2 Lout).
        let out_l = Self::mul_q15(mdb0, fb_x) + fb_b0;
        let out_r = Self::mul_q15(mdb1, fb_x) + fb_b1;
        self.reverb.wet_l = Self::scale_reverb_output(out_l, self.reverb_vol_l.current);
        self.reverb.wet_r = Self::scale_reverb_output(out_r, self.reverb_vol_r.current);
    }

    // ============================================================
    //  Per-sample tick -- called from the bus scheduler.
    // ============================================================

    /// Produce one stereo sample's worth of audio. Called from the bus
    /// each time `EventSlot::SpuAsync` fires. Returns the number of
    /// samples produced (currently always 1 -- future batching could
    /// amortise voice-state fetches across several samples).
    pub fn tick_sample(&mut self, now: u64) -> usize {
        self.last_sample_cycle = now;
        if let Some((pending, deadline)) = self.spustat_control_pending {
            if deadline <= now {
                self.spustat_control = pending;
                self.spustat_control_pending = None;
                if (pending >> 4) & 3 == 1 {
                    self.drain_transfer_fifo_to_ram();
                }
            }
        }
        // KON / KOFF are applied at the END of this tick (after the
        // sample is emitted), not here -- see the apply_kon_koff() call
        // below. PSX-SPX runs update_keystatus() after push_sample
        // (spu.rs:577-580) and PSX-SPX runs KeyOff/KeyOn after
        // WriteToCaptureBuffer (-2558, gated on i==0); per
        // PSX-SPX a KON/KOFF write is latched and acted on at the next
        // 44.1 kHz tick, so a keyed-on voice's first Attack sample lands
        // on the sample AFTER the one in progress.

        // 1b. Advance noise generator -- one LFSR-step pass per
        //     sample (the noise_tick's internal counter gates
        //     actual register updates).
        self.noise_tick();

        // 1c. Advance the global volume sweeps. Per-voice volume sweeps
        //     are ticked in `tick_voice` after each voice's sample is
        //     applied (apply-then-tick, matching PSX-SPX).
        //     CD/external/reverb volumes are fixed (`write_signed_q15`,
        //     a no-op tick); main volume may be sweep-programmed.
        self.main_vol_l.tick();
        self.main_vol_r.tick();
        self.cd_vol_l.tick();
        self.cd_vol_r.tick();
        self.ext_vol_l.tick();
        self.ext_vol_r.tick();
        self.reverb_vol_l.tick();
        self.reverb_vol_r.tick();

        // 2. For each voice, step envelope + ADPCM playback, accumulate
        //    stereo contribution. Modulator voices (the N-1 voice when
        //    PMon bit N is set) update `last_sample` for the modulated
        //    voice's FMod read, but their own L/R contribution is
        //    **suppressed** from the audible mix -- matches Redux's
        //    `if (FMod == 2) iFMod[ns] = sval; else { SSumL/R += ... }`
        //    branch (`spu.cc:689`).
        let mut sum_l: i32 = 0;
        let mut sum_r: i32 = 0;
        let mut reverb_in_l: i32 = 0;
        let mut reverb_in_r: i32 = 0;
        for v in 0..NUM_VOICES {
            let (l, r) = self.tick_voice(v);
            if l != 0 || r != 0 {
                self.dbg_voiced_samples[v] = self.dbg_voiced_samples[v].saturating_add(1);
            }
            let is_modulator = v + 1 < NUM_VOICES && (self.pmon & (1 << (v + 1))) != 0;
            if !is_modulator {
                sum_l = sum_l.saturating_add(l as i32);
                sum_r = sum_r.saturating_add(r as i32);
                if self.reverb_on & (1 << v) != 0 {
                    reverb_in_l = reverb_in_l.saturating_add(l as i32);
                    reverb_in_r = reverb_in_r.saturating_add(r as i32);
                }
            }
        }
        self.dbg_sample_idx = self.dbg_sample_idx.wrapping_add(1);
        self.dbg_pmon_ever |= self.pmon;
        self.dbg_noise_ever |= self.noise_on;

        // 3. Mix CD audio input at CD_VOL_L/R. Source is the CDROM's
        //    CD-DA sample stream or the decoded XA-ADPCM payload,
        //    both fed via [`Spu::feed_cd_audio`]. When the queue is
        //    empty, CD contribution is zero -- matches real hardware
        //    where "no CD playing" means no CD input signal.
        // CD post-volume samples. These are also tapped into the CD-L/R
        // capture buffers (PSX-SPX: SPU RAM 0x000/0x400 capture the CD
        // input *after* the CD-input volume), independently of whether the
        // CD input is routed to the main mix or the reverb bus.
        let mut cd_cap_l: i32 = 0;
        let mut cd_cap_r: i32 = 0;
        if let Some((cd_l, cd_r)) = self.cd_audio_in.pop_front() {
            // CD_VOL regs are Q15 signed -- range -0x8000..=0x7FFF.
            // `>> 15` brings them back to i16 scale. Always consume
            // the stream so timing stays live while muted/disabled;
            // only route it into the mixer when SPUCNT bit 0 is set.
            let cl = ((cd_l as i32) * self.cd_vol_l.current as i32) >> 15;
            let cr = ((cd_r as i32) * self.cd_vol_r.current as i32) >> 15;
            cd_cap_l = cl;
            cd_cap_r = cr;
            if self.spucnt & SPUCNT_CD_AUDIO_ENABLE != 0 {
                sum_l = sum_l.saturating_add(cl);
                sum_r = sum_r.saturating_add(cr);
            }
            if self.spucnt & SPUCNT_CD_REVERB_ENABLE != 0 {
                reverb_in_l = reverb_in_l.saturating_add(cl);
                reverb_in_r = reverb_in_r.saturating_add(cr);
            }
        }
        // External-audio input is not wired (no hardware source
        // available on a closed console); EXT_VOL_L/R are stored for
        // round-trip reads only.

        // 3b. Write the four SPU capture buffers (PSX-SPX). The SPU mirrors
        //     CD-L, CD-R, Voice1 and Voice3 into SPU RAM 0x000/0x400/
        //     0x800/0xC00 every sample, advancing a shared 0x400-byte ring
        //     index. Games read these back for CD-DA sync and audio
        //     visualisers, and an SPU IRQ armed on the capture region
        //     latches from these writes. Voice1/Voice3 use their post-ADSR
        //     `last_sample` (already i16); CD-L/R saturate the post-volume
        //     value to i16.
        self.write_to_capture(0, saturate_i16(cd_cap_l) as u16);
        self.write_to_capture(1, saturate_i16(cd_cap_r) as u16);
        self.write_to_capture(2, self.voices[1].last_sample as u16);
        self.write_to_capture(3, self.voices[3].last_sample as u16);
        self.capture_buffer_pos = (self.capture_buffer_pos + 2) & 0x3FF;
        // SB4 silicon 2026-08-07: bit 11 = 1 while the second half is being
        // written, on SCPH-9902 too (edge-keyed measurement with both
        // phases exercised). The previous 9902 inversion came from a single
        // snapshot against a free-running ring index.
        if self.capture_buffer_pos >= 0x200 {
            self.spustat |= 1 << 11;
        } else {
            self.spustat &= !(1 << 11);
        }

        // 4. Process the wet reverb bus, then apply MAIN VOLUME as the final
        //    stage. PSX-SPX both scale the clamped (dry+wet)
        //    sum by the raw signed-Q15 main-volume register: `(s * vol) >> 15`.
        //    PSoXide previously dropped this (claiming PCSX-Redux did too), which
        //    left output ~2x too loud and clipped the un-attenuated 24-voice sum
        //    far more often than hardware or either oracle.
        let (wet_l, wet_r) = self.mix_reverb(reverb_in_l, reverb_in_r);
        let dry_l = sum_l;
        let dry_r = sum_r;
        let mixed_l = saturate_i16(dry_l.saturating_add(wet_l)) as i32;
        let mixed_r = saturate_i16(dry_r.saturating_add(wet_r)) as i32;
        let out_l = saturate_i16((mixed_l * self.main_vol_l.raw as i16 as i32) >> 15);
        let out_r = saturate_i16((mixed_r * self.main_vol_r.raw as i16 as i32) >> 15);
        self.dbg_dry_energy = self
            .dbg_dry_energy
            .saturating_add(dry_l.unsigned_abs() as u64 + dry_r.unsigned_abs() as u64);
        self.dbg_wet_energy = self
            .dbg_wet_energy
            .saturating_add(wet_l.unsigned_abs() as u64 + wet_r.unsigned_abs() as u64);

        // 5. Push to output ring, discarding oldest if full.
        if self.audio_out.len() >= OUTPUT_BUFFER_CAP {
            self.audio_out.pop_front();
        }
        self.audio_out.push_back((out_l, out_r));

        self.tick_decode_buffer_irq();

        // 6. Apply pending KON / KOFF AFTER this sample was emitted and
        //    the capture buffer was written, matching PSX-SPX
        //    update_keystatus() (spu.rs:580) and the hardware post-
        //    WriteToCaptureBuffer KeyOff/KeyOn (-2558). The
        //    keyed voice therefore first contributes on the NEXT tick.
        self.apply_kon_koff();
        self.samples_produced = self.samples_produced.saturating_add(1);
        1
    }

    fn tick_decode_buffer_irq(&mut self) {
        if self.irq_triggerable() && self.irq_addr < 0x1000 {
            for bank in 0..4 {
                let cursor = self.decode_irq_cursor + bank * 0x400;
                if self.irq_addr >= cursor && self.irq_addr < cursor + 2 {
                    self.spustat |= 1 << 6;
                    self.irq_pending = true;
                    break;
                }
            }
        }

        self.decode_irq_cursor += 2;
        if self.decode_irq_cursor > 0x3ff {
            self.decode_irq_cursor = 0;
        }
    }

    fn apply_kon_koff(&mut self) {
        let kon = std::mem::take(&mut self.kon_pending);
        let koff = std::mem::take(&mut self.koff_pending);
        for v in 0..NUM_VOICES {
            let bit = 1u32 << v;
            let key_on = kon & bit != 0;
            if key_on {
                self.voices[v].key_on();
                self.dbg_kon_count[v] = self.dbg_kon_count[v].saturating_add(1);
                let vc = &self.voices[v];
                let cfg = (
                    vc.start_addr,
                    vc.adsr_lo,
                    vc.adsr_hi,
                    vc.raw_pitch,
                    vc.vol_l.current,
                    vc.vol_r.current,
                );
                self.dbg_keyon_cfg[v] = cfg;
            }
            if !key_on && koff & bit != 0 {
                self.voices[v].key_off();
                self.dbg_koff_count[v] = self.dbg_koff_count[v].saturating_add(1);
            }
        }
    }

    /// Advance one voice by one output sample. Returns `(l, r)`
    /// pre-main-volume, post-voice-volume contribution in i16 scale.
    fn tick_voice(&mut self, v: usize) -> (i16, i16) {
        // SB4 silicon 2026-08-07: a freshly keyed voice is silent, envelope
        // included, for ~7 ticks after KON lands before it starts stepping.
        if self.voices[v].start_delay > 0 {
            self.voices[v].start_delay -= 1;
            self.voices[v].last_sample = 0;
            return (0, 0);
        }
        // Fetch raw sample using the SPU's Gaussian interpolation path.
        let sample_i16 = self.fetch_voice_sample(v);

        // Advance ADSR envelope.
        let env = self.voices[v].step_envelope();
        let mixed_i16 = apply_adsr_volume(sample_i16, env);

        // Diagnostic trace: track per-window max decode amplitude + envelope,
        // snapshot every 1024 samples to bisect premature cutoffs
        // (decode-silence with envelope held vs envelope-drop).
        let s_abs = (sample_i16 as i32).abs();
        if s_abs > self.dbg_acc_smax[v] {
            self.dbg_acc_smax[v] = s_abs;
        }
        if env > self.dbg_acc_emax[v] {
            self.dbg_acc_emax[v] = env;
        }
        if self.dbg_sample_idx & 0x3FF == 0 && self.dbg_trace[v].len() < 2600 {
            let ph = self.voices[v].phase as u8;
            self.dbg_trace[v].push((self.dbg_acc_smax[v] as i16, self.dbg_acc_emax[v], ph));
            self.dbg_acc_smax[v] = 0;
            self.dbg_acc_emax[v] = 0;
        }

        let voice = &mut self.voices[v];
        voice.last_sample = mixed_i16;

        // Apply per-voice L / R volumes in full signed Q15 (sample *
        // current_level >> 15), matching PSX-SPX
        // `apply_volume`. `current` is the live sweep/fixed level, so
        // sweep-configured voices fade and negative-phase volumes
        // invert correctly. Tick the sweep AFTER applying this sample,
        // mirroring both oracles' apply-then-`tick` order.
        let l = ((mixed_i16 as i32) * voice.vol_l.current as i32) >> 15;
        let r = ((mixed_i16 as i32) * voice.vol_r.current as i32) >> 15;
        voice.vol_l.tick();
        voice.vol_r.tick();
        (saturate_i16(l), saturate_i16(r))
    }

    /// Fetch the current voice's interpolated sample. This mirrors
    /// Redux's `spos` + `StoreInterpolationVal` flow: consume decoded
    /// samples into a rolling 4-sample ring while `sample_pos >=
    /// 0x10000`, then run the Gaussian window over that ring using the
    /// remaining fractional position.
    fn fetch_voice_sample(&mut self, v: usize) -> i16 {
        // Voices in Off contribute nothing.
        if self.voices[v].phase == AdsrPhase::Off {
            return 0;
        }
        let noise_mode = self.noise_on & (1 << v) != 0;
        let feeds_fmod = v + 1 < NUM_VOICES && (self.pmon & (1 << (v + 1))) != 0;
        let mute_voice_sample = (self.spucnt & SPUCNT_UNMUTE) == 0 && !feeds_fmod;

        // Determine effective pitch. PMOn: voice N takes its pitch
        // from voice N-1's most recent post-ADSR sample. Formula is
        // Redux's `FModChangeFrequency` (spu.cc:266):
        //
        //     NP = ((32768 + iFMod[ns]) * raw_pitch) / 32768
        //     NP = clamp(NP, 1, 0x3FFF)
        //
        // Voice 0 cannot be modulated (no preceding voice). The
        // modulator voice's own L/R output is suppressed from the
        // audible mix in `tick_sample`.
        // The sample-rate register stores the full 16-bit written value so
        // reads echo it back like hardware; the rate counter clamps to
        // 0x3FFF here (values above that don't speed playback further).
        let mut pitch = (self.voices[v].raw_pitch as u32).min(0x3FFF);
        if v > 0 && self.pmon & (1 << v) != 0 {
            let prev = self.voices[v - 1].last_sample as i32;
            let np = ((0x8000 + prev) * pitch as i32) / 0x8000;
            pitch = (np.clamp(1, 0x3FFF)) as u32;
        }
        if pitch == 0 {
            pitch = 1;
        }

        // Consume decoded samples into the interpolation ring until
        // the fixed-point cursor is back inside the current source
        // sample. This preserves the previous block's tail across
        // ADPCM boundaries instead of substituting zeros.
        while self.voices[v].sample_pos >= 0x10000 {
            if self.voices[v].sample_index >= ADPCM_SAMPLES_PER_BLOCK {
                // Block boundary: the current block's 28 samples have all
                // been consumed. If it was a loop-end block, latch ENDX now
                // -- after the block finished playing -- even if the voice
                // is about to stop. This matches PSX-SPX,
                // which set ENDX at the boundary crossing (using the just-
                // finished block's flags), not when the loop-end block was
                // first decoded a block earlier.
                if self.voices[v].endx_pending {
                    self.endx_latched |= 1 << v;
                    self.voices[v].endx_pending = false;
                }
                if self.voices[v].stop_after_block {
                    self.dbg_sampstop_count[v] = self.dbg_sampstop_count[v].saturating_add(1);
                    let voice = &mut self.voices[v];
                    voice.phase = AdsrPhase::Off;
                    voice.envelope = 0;
                    voice.stop_after_block = false;
                    voice.last_sample = 0;
                    return 0;
                }
                self.decode_next_block(v);
            }
            let voice = &mut self.voices[v];
            let sample = if mute_voice_sample {
                0
            } else {
                // Redux applies SPUCNT's mute bit before storing into
                // the interpolation history, and clamps the raw decoded
                // value to -32767..32767 on that same path.
                voice.sample_buf[voice.sample_index].clamp(-32767, 32767) as i16
            };
            voice.sample_index += 1;
            voice.push_interpolation_sample(sample);
            voice.sample_pos -= 0x10000;
        }

        let out = if noise_mode {
            // Redux still advances the sample cursor / decode state for
            // noise voices, but substitutes the final audible sample
            // with the shared noise generator output.
            self.noise_val
        } else {
            let window = self.voices[v].interpolation_window();
            gauss_interpolate(window, self.voices[v].sample_pos)
        };
        self.voices[v].sample_pos = self.voices[v].sample_pos.saturating_add(pitch << 4);
        out
    }

    /// Decode the next 16-byte ADPCM block at `current_addr` into the
    /// voice's sample buffer, update `s_1`/`s_2` filter history, handle
    /// loop flags, and advance `current_addr` to the following block.
    /// On a flag-1 terminator the voice either loops to `loop_addr` or
    /// stops playing.
    fn decode_next_block(&mut self, v: usize) {
        // Snapshot voice state we need for decoding.
        let current = self.voices[v].current_addr;
        let irq_enabled = self.irq_triggerable();
        let irq_target = self.irq_addr & !0xF;

        // IRQ match check: if the block being decoded covers the IRQ
        // address, raise SPU IRQ.
        if irq_enabled && (current & !0xF) == irq_target {
            self.spustat |= 1 << 6;
            self.irq_pending = true;
        }

        // Read block header + flags + 14 data bytes from SPU RAM.
        let block = read_adpcm_block(&self.ram[..], current);

        let predictor = (block[0] >> 4) as usize;
        let predictor = predictor.min(ADPCM_FILTER_TABLE.len() - 1);
        // Reserved shift values 13..15 act the same as shift=9 on real
        // hardware (nocash PSX-SPX; PSX-SPX `ADPCMBlock::GetShift`,
        // PSX-SPX `decode_block`). Clamping to 12 instead would attenuate
        // those nibbles by ~3 extra bits, so map >12 to 9.
        let raw_shift = block[0] & 0x0F;
        let shift = (if raw_shift > 12 { 9 } else { raw_shift }) as u32;
        let flags = block[1];

        // Decode 28 samples (4-bit nibbles, little-endian within bytes:
        // byte[n] low nibble → sample 2n, high nibble → sample 2n+1).
        let voice = &mut self.voices[v];
        let (f1, f2) = ADPCM_FILTER_TABLE[predictor];
        for i in 0..ADPCM_SAMPLES_PER_BLOCK {
            let byte = block[2 + (i >> 1)] as i32;
            let nibble = if i & 1 == 0 {
                byte & 0x0F
            } else {
                (byte >> 4) & 0x0F
            };
            // Decode path:
            //   s   = sign_extend_4bit(nibble) << 12
            //   raw = s >> shift_factor
            //   fa  = raw + (s_1*f1 + s_2*f2) >> 6
            // The 4-bit nibble is sign-extended then scaled by the header
            // shift (smaller shift = louder), matching real hardware.
            let signed = ((nibble << 28) >> 28) << 12;
            let raw = signed >> shift;
            let fa = raw + ((voice.s_1 * f1) >> 6) + ((voice.s_2 * f2) >> 6);
            // Saturate to 16 bits BEFORE feeding the IIR predictor history,
            // exactly like hardware: nocash `old = MinMax(sample, -8000h,
            // +7FFFh)`, PSX-SPX `Clamp16(sample)`, PSX-SPX
            // `sample.clamp(-0x8000, 0x7fff)`. Storing the
            // unclamped i32 let prev1/prev2 drift on loud/bass voices.
            let clamped = fa.clamp(-0x8000, 0x7FFF);
            voice.sample_buf[i] = clamped;
            voice.s_2 = voice.s_1;
            voice.s_1 = clamped;
        }
        voice.sample_index = 0;
        // Count blocks decoded since key-on. The first decoded block is the
        // voice's "first block" (decoded_block_count == 1) -- the window in
        // which a REPEAT_ADDR write must not lock the loop address (see
        // write_voice_reg / PSX-SPX first-block).
        voice.decoded_block_count = voice.decoded_block_count.saturating_add(1);

        // Advance current_addr for the next block.
        let block_bytes = ADPCM_BLOCK_BYTES as u32;
        let next_addr = (current + block_bytes) & (SPU_RAM_BYTES as u32 - 1);

        // Handle ADPCM block flags:
        //   bit 0 (flag 1) -- end of sample: jump to loop_addr on next
        //     block (if flag 2 is set) or stop the voice (flag 2 clear).
        //   bit 1 (flag 2) -- repeat: suppresses stop on flag 1.
        //   bit 2 (flag 4) -- loop-start: updates loop_addr to this
        //     block's address (unless software has locked it via
        //     REPEAT_ADDR write).
        if flags & 0x4 != 0 && !voice.loop_addr_locked {
            voice.loop_addr = current;
            voice.loop_addr_raw = (current >> 3) as u16;
        }
        if flags & 0x1 != 0 {
            // Loop-end (bit 0). Defer the ENDX latch to the next block
            // boundary (it must fire only after this block's 28 samples
            // have played -- flushed in fetch_voice_sample), and redirect
            // playback to the loop address unconditionally, exactly as
            // PSX-SPX do on loop_end. The voice is stopped
            // only when the repeat bit (bit 1) is clear -- tested on its
            // own, not via the whole-byte `flags == 0x3` value that
            // PEOPS/PCSX-Redux used as a loop-hang guard, which wrongly
            // force-killed single-block loops encoded as 0x7. A noise
            // voice is never stopped by these flags.
            let noise = self.noise_on & (1 << v) != 0;
            let voice = &mut self.voices[v];
            voice.endx_pending = true;
            voice.current_addr = voice.loop_addr;
            voice.stop_after_block = (flags & 0x2 == 0) && !noise;
        } else {
            voice.current_addr = next_addr;
            voice.stop_after_block = false;
        }
    }
}

// ===============================================================
//  Gaussian interpolation table (PSX hardware).
// ===============================================================

/// 512-entry PSX hardware Gaussian interpolation coefficient table
/// (the nocash PSX-SPX table). Byte-identical to the tables shipped by
/// PSX-SPX (``) and PSX-SPX
/// (``); the first 16 entries are -1 and the
/// peak coefficient is `GAUSS_TABLE[0x1FF] == 0x59B3`. Indexed in a
/// butterfly by the 8-bit interpolation phase `i = (frac >> 8) & 0xFF`
/// via taps `T[0xFF-i], T[0x1FF-i], T[0x100+i], T[i]`; each product
/// with an i16 sample is accumulated in i32 and the sum is shifted
/// right by 15. Per-phase 4-tap sum is ~0x7F80 (the real SPU's
/// deliberate ~0.4% gain droop), so unity-DC output is ~32639, not
/// full scale. The previous build shipped the legacy PEOPS/old-PCSX
/// 11-bit table (peak 0x519, `>> 11`, `& !2047`), a different curve.
const GAUSS_TABLE: [i32; 0x200] = [
    -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001,
    -0x001, -0x001, -0x001, -0x001, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0001,
    0x0001, 0x0001, 0x0001, 0x0002, 0x0002, 0x0002, 0x0003, 0x0003, 0x0003, 0x0004, 0x0004, 0x0005,
    0x0005, 0x0006, 0x0007, 0x0007, 0x0008, 0x0009, 0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x000E,
    0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x0015, 0x0016, 0x0018, 0x0019, 0x001B, 0x001C, 0x001E,
    0x0020, 0x0021, 0x0023, 0x0025, 0x0027, 0x0029, 0x002C, 0x002E, 0x0030, 0x0033, 0x0035, 0x0038,
    0x003A, 0x003D, 0x0040, 0x0043, 0x0046, 0x0049, 0x004D, 0x0050, 0x0054, 0x0057, 0x005B, 0x005F,
    0x0063, 0x0067, 0x006B, 0x006F, 0x0074, 0x0078, 0x007D, 0x0082, 0x0087, 0x008C, 0x0091, 0x0096,
    0x009C, 0x00A1, 0x00A7, 0x00AD, 0x00B3, 0x00BA, 0x00C0, 0x00C7, 0x00CD, 0x00D4, 0x00DB, 0x00E3,
    0x00EA, 0x00F2, 0x00FA, 0x0101, 0x010A, 0x0112, 0x011B, 0x0123, 0x012C, 0x0135, 0x013F, 0x0148,
    0x0152, 0x015C, 0x0166, 0x0171, 0x017B, 0x0186, 0x0191, 0x019C, 0x01A8, 0x01B4, 0x01C0, 0x01CC,
    0x01D9, 0x01E5, 0x01F2, 0x0200, 0x020D, 0x021B, 0x0229, 0x0237, 0x0246, 0x0255, 0x0264, 0x0273,
    0x0283, 0x0293, 0x02A3, 0x02B4, 0x02C4, 0x02D6, 0x02E7, 0x02F9, 0x030B, 0x031D, 0x0330, 0x0343,
    0x0356, 0x036A, 0x037E, 0x0392, 0x03A7, 0x03BC, 0x03D1, 0x03E7, 0x03FC, 0x0413, 0x042A, 0x0441,
    0x0458, 0x0470, 0x0488, 0x04A0, 0x04B9, 0x04D2, 0x04EC, 0x0506, 0x0520, 0x053B, 0x0556, 0x0572,
    0x058E, 0x05AA, 0x05C7, 0x05E4, 0x0601, 0x061F, 0x063E, 0x065C, 0x067C, 0x069B, 0x06BB, 0x06DC,
    0x06FD, 0x071E, 0x0740, 0x0762, 0x0784, 0x07A7, 0x07CB, 0x07EF, 0x0813, 0x0838, 0x085D, 0x0883,
    0x08A9, 0x08D0, 0x08F7, 0x091E, 0x0946, 0x096F, 0x0998, 0x09C1, 0x09EB, 0x0A16, 0x0A40, 0x0A6C,
    0x0A98, 0x0AC4, 0x0AF1, 0x0B1E, 0x0B4C, 0x0B7A, 0x0BA9, 0x0BD8, 0x0C07, 0x0C38, 0x0C68, 0x0C99,
    0x0CCB, 0x0CFD, 0x0D30, 0x0D63, 0x0D97, 0x0DCB, 0x0E00, 0x0E35, 0x0E6B, 0x0EA1, 0x0ED7, 0x0F0F,
    0x0F46, 0x0F7F, 0x0FB7, 0x0FF1, 0x102A, 0x1065, 0x109F, 0x10DB, 0x1116, 0x1153, 0x118F, 0x11CD,
    0x120B, 0x1249, 0x1288, 0x12C7, 0x1307, 0x1347, 0x1388, 0x13C9, 0x140B, 0x144D, 0x1490, 0x14D4,
    0x1517, 0x155C, 0x15A0, 0x15E6, 0x162C, 0x1672, 0x16B9, 0x1700, 0x1747, 0x1790, 0x17D8, 0x1821,
    0x186B, 0x18B5, 0x1900, 0x194B, 0x1996, 0x19E2, 0x1A2E, 0x1A7B, 0x1AC8, 0x1B16, 0x1B64, 0x1BB3,
    0x1C02, 0x1C51, 0x1CA1, 0x1CF1, 0x1D42, 0x1D93, 0x1DE5, 0x1E37, 0x1E89, 0x1EDC, 0x1F2F, 0x1F82,
    0x1FD6, 0x202A, 0x207F, 0x20D4, 0x2129, 0x217F, 0x21D5, 0x222C, 0x2282, 0x22DA, 0x2331, 0x2389,
    0x23E1, 0x2439, 0x2492, 0x24EB, 0x2545, 0x259E, 0x25F8, 0x2653, 0x26AD, 0x2708, 0x2763, 0x27BE,
    0x281A, 0x2876, 0x28D2, 0x292E, 0x298B, 0x29E7, 0x2A44, 0x2AA1, 0x2AFF, 0x2B5C, 0x2BBA, 0x2C18,
    0x2C76, 0x2CD4, 0x2D33, 0x2D91, 0x2DF0, 0x2E4F, 0x2EAE, 0x2F0D, 0x2F6C, 0x2FCC, 0x302B, 0x308B,
    0x30EA, 0x314A, 0x31AA, 0x3209, 0x3269, 0x32C9, 0x3329, 0x3389, 0x33E9, 0x3449, 0x34A9, 0x3509,
    0x3569, 0x35C9, 0x3629, 0x3689, 0x36E8, 0x3748, 0x37A8, 0x3807, 0x3867, 0x38C6, 0x3926, 0x3985,
    0x39E4, 0x3A43, 0x3AA2, 0x3B00, 0x3B5F, 0x3BBD, 0x3C1B, 0x3C79, 0x3CD7, 0x3D35, 0x3D92, 0x3DEF,
    0x3E4C, 0x3EA9, 0x3F05, 0x3F62, 0x3FBD, 0x4019, 0x4074, 0x40D0, 0x412A, 0x4185, 0x41DF, 0x4239,
    0x4292, 0x42EB, 0x4344, 0x439C, 0x43F4, 0x444C, 0x44A3, 0x44FA, 0x4550, 0x45A6, 0x45FC, 0x4651,
    0x46A6, 0x46FA, 0x474E, 0x47A1, 0x47F4, 0x4846, 0x4898, 0x48E9, 0x493A, 0x498A, 0x49D9, 0x4A29,
    0x4A77, 0x4AC5, 0x4B13, 0x4B5F, 0x4BAC, 0x4BF7, 0x4C42, 0x4C8D, 0x4CD7, 0x4D20, 0x4D68, 0x4DB0,
    0x4DF7, 0x4E3E, 0x4E84, 0x4EC9, 0x4F0E, 0x4F52, 0x4F95, 0x4FD7, 0x5019, 0x505A, 0x509A, 0x50DA,
    0x5118, 0x5156, 0x5194, 0x51D0, 0x520C, 0x5247, 0x5281, 0x52BA, 0x52F3, 0x532A, 0x5361, 0x5397,
    0x53CC, 0x5401, 0x5434, 0x5467, 0x5499, 0x54CA, 0x54FA, 0x5529, 0x5558, 0x5585, 0x55B2, 0x55DE,
    0x5609, 0x5632, 0x565B, 0x5684, 0x56AB, 0x56D1, 0x56F6, 0x571B, 0x573E, 0x5761, 0x5782, 0x57A3,
    0x57C3, 0x57E2, 0x57FF, 0x581C, 0x5838, 0x5853, 0x586D, 0x5886, 0x589E, 0x58B5, 0x58CB, 0x58E0,
    0x58F4, 0x5907, 0x5919, 0x592A, 0x593A, 0x5949, 0x5958, 0x5965, 0x5971, 0x597C, 0x5986, 0x598F,
    0x5997, 0x599E, 0x59A4, 0x59A9, 0x59AD, 0x59B0, 0x59B2, 0x59B3,
];

/// Sample four points through the hardware Gaussian table at the
/// current fractional position, matching PSX-SPX `Voice::interpolate`
/// (-614) and PSX-SPX `Interpolate` (-2040).
/// `samples` is the rolling ring window `[oldest, older, newer, newest]`
/// (== PSX-SPX `s[-3..=0]`); `frac` is the 16.16 fixed-point cursor
/// remainder (nominally `0..0xFFFF`).
///
/// The 8-bit phase `i = (frac >> 8) & 0xFF` is exactly PSoXide's prior
/// phase selector: `((frac >> 6) & !3) >> 2 == (frac >> 8) & 0xFF` for
/// every `frac` (verified across all 0x10000 values), so the 16x
/// fixed-point scale is unchanged -- only the table and the
/// butterfly/`>> 15` arithmetic differ. The `& 0xFF` also bounds every
/// table access to 0..=0x1FF, keeping out-of-range `frac` panic-free.
fn gauss_interpolate(samples: [i16; 4], frac: u32) -> i16 {
    let i = ((frac >> 8) & 0xFF) as usize;
    let mut out = GAUSS_TABLE[0xFF - i] * samples[0] as i32;
    out += GAUSS_TABLE[0x1FF - i] * samples[1] as i32;
    out += GAUSS_TABLE[0x100 + i] * samples[2] as i32;
    out += GAUSS_TABLE[i] * samples[3] as i32;
    saturate_i16(out >> 15)
}

// ===============================================================
//  XA ADPCM decoder.
// ===============================================================

// ===============================================================
//  Helpers.
// ===============================================================

// `decode_volume` has been subsumed by `VolumeEnvelope::write`, which
// both snaps static levels AND starts sweep animations. The per-
// write decode + animate path is now centralised so every volume
// register (voice L/R × 24, main L/R, CD L/R, ext L/R, reverb L/R)
// shares the same behaviour.

/// Decode a voice-bank byte address into `(voice_index, byte_offset)`.
fn decode_voice(phys: u32) -> Option<(usize, u32)> {
    if !(VOICE_BASE..VOICE_END).contains(&phys) {
        return None;
    }
    let rel = phys - VOICE_BASE;
    Some(((rel / 16) as usize, rel % 16))
}

/// Read one ADPCM block (16 bytes) from SPU RAM at the given byte
/// address. Wraps modulo the RAM size.
fn read_adpcm_block(ram: &[u16], addr: u32) -> [u8; ADPCM_BLOCK_BYTES] {
    let mut out = [0u8; ADPCM_BLOCK_BYTES];
    let base = (addr & (SPU_RAM_BYTES as u32 - 1)) as usize;
    for (i, out_byte) in out.iter_mut().enumerate().take(ADPCM_BLOCK_BYTES) {
        let byte_addr = (base + i) & (SPU_RAM_BYTES - 1);
        let halfword = ram[byte_addr >> 1];
        *out_byte = if byte_addr & 1 == 0 {
            halfword as u8
        } else {
            (halfword >> 8) as u8
        };
    }
    out
}

fn apply_adsr_volume(sample: i16, envelope: i32) -> i16 {
    // SB4 silicon 2026-08-07: the voice output is the full 15-bit envelope
    // applied as (sample * env) >> 15 with floor rounding. Seven
    // consecutive attack/decay/sustain onset samples in the NOISE capture
    // match this exactly; the previous (env>>5)/1023 form was off by 1-2
    // LSB on five of them. No saturation needed: |result| <= 32767, and
    // Rust's >> on i32 is arithmetic, matching the floor behavior the
    // 0x3FFF decay sample confirms.
    let env = envelope.clamp(0, 0x7FFF);
    (((sample as i32) * env) >> 15) as i16
}

/// Clamp a 32-bit sample to signed 16-bit range.
fn saturate_i16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

// ===============================================================
//  Tests.
// ===============================================================

#[cfg(test)]
#[cfg(test)]
mod tests;
