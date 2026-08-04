// SPDX-License-Identifier: GPL-2.0-or-later
//! High-level PS1 SPU (Sound Processing Unit) API.
//!
//! Turns SPU-programming-by-magic-numbers into typed primitives:
//!
//! - [`Voice`] -- typed voice index in `0..24`.
//! - [`Pitch`] -- Q5.12 sample-rate multiplier; `Pitch::UNITY` =
//!   44100 Hz (one sample per SPU tick).
//! - [`Volume`] -- per-voice / main-output signed linear gain.
//! - [`CdVolume`] -- CD-DA / XA input signed Q15 gain.
//! - [`Adsr`] -- typed envelope descriptor; builds the two-word SPU
//!   envelope register pair.
//! - [`SpuAddr`] -- 8-byte-aligned SPU RAM pointer (the only shape
//!   voice-start / loop / transfer registers accept).
//!
//! Typical boot sequence:
//!
//! ```ignore
//! spu::init();                              // turn the SPU on, sensible defaults
//! spu::set_main_volume(Volume::MAX, Volume::MAX);
//! spu::upload_adpcm(SpuAddr::new(0x1010), &TONE_SAMPLE);
//! let v = Voice::V0;
//! v.set_volume(Volume::MAX, Volume::MAX);
//! v.set_pitch(Pitch::UNITY);
//! v.set_start_addr(SpuAddr::new(0x1010));
//! v.set_adsr(Adsr::default_tone());
//! Voice::key_on(v.mask());                  // start the tone
//! ```
//!
//! ## What this crate owns vs doesn't
//!
//! - **Owns**: typed register access, key-on/off masks, the ADPCM
//!   byte-stream upload path, ADSR encoding helpers, sample-start
//!   address alignment.
//! - **Doesn't own (yet)**: ADPCM encoder (ship pre-baked samples
//!   instead -- see `vendor/tone_*.adpcm`, or cook `.psau` with
//!   `psxed audio-pack`), reverb preset tables, DMA-based sample
//!   upload. Those land as the ladder pulls them in.
//!
//! ## Q-format conventions
//!
//! - **Pitch**: Q5.12 in a u16. `0x1000` = 1.0 = the sample plays
//!   at its recorded rate (44100 Hz). Halving to `0x0800` drops an
//!   octave; `0x2000` raises one. Pitch field is 14 bits on
//!   hardware; values above `0x3FFF` clamp.
//! - **Volumes**: i16. Sign carries, the magnitude is linear. Max
//!   output is `0x3FFF` (= 1.0); `Volume::MAX` already uses this.
//!   Negative means "phase-inverted," used for stereo tricks we
//!   don't wire up here yet.
//! - **CD volumes**: signed Q15. Max input gain is `0x7FFF`, exposed
//!   as `CdVolume::MAX`.
//! - **SPU RAM addresses**: stored in registers as `addr / 8`,
//!   [`SpuAddr`] does the divide so the caller passes byte offsets.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_io::dma::{self, Channel};
use psx_io::spu::{SPUCNT, SPUSTAT, SPU_BASE};

pub mod tones;

// ======================================================================
// MMIO helpers -- hand-rolled volatile access at fixed SPU offsets
// ======================================================================

/// Raw MMIO write of a 16-bit SPU register.
///
/// Every SPU register is 16-bit (the controller doesn't care about
/// higher-width accesses -- the lower half is what matters). We go
/// through a volatile pointer so the compiler can't reorder writes
/// across register boundaries.
#[inline]
fn write_reg16(addr: u32, value: u16) {
    // SAFETY: SPU registers live in the hardware-MMIO window at
    // 0x1F80_1C00..0x1F80_1FFC. The SDK only exposes functions
    // that compute their `addr` from typed handles whose
    // constructors validate ranges, so no caller can point this at
    // non-SPU memory.
    unsafe {
        core::ptr::write_volatile(addr as *mut u16, value);
    }
}

/// Raw MMIO read of a 16-bit SPU register.
#[inline]
fn read_reg16(addr: u32) -> u16 {
    // SAFETY: same MMIO contract as `write_reg16`.
    unsafe { core::ptr::read_volatile(addr as *const u16) }
}

// Voice register block is 16 bytes per voice starting at 0x1F80_1C00.
const VOICE_STRIDE: u32 = 0x10;
const VOICE_VOL_LEFT: u32 = 0x0;
const VOICE_VOL_RIGHT: u32 = 0x2;
const VOICE_PITCH: u32 = 0x4;
const VOICE_START_ADDR: u32 = 0x6;
const VOICE_ADSR_LO: u32 = 0x8;
const VOICE_ADSR_HI: u32 = 0xA;
// 0xC = current ADSR envelope (read-only).
const VOICE_REPEAT_ADDR: u32 = 0xE;

// Global registers (PSX-SPX § "SPU Registers").
const MAIN_VOL_LEFT: u32 = 0x1F80_1D80;
const MAIN_VOL_RIGHT: u32 = 0x1F80_1D82;
const REVERB_VOL_LEFT: u32 = 0x1F80_1D84;
const REVERB_VOL_RIGHT: u32 = 0x1F80_1D86;
const KEY_ON_LO: u32 = 0x1F80_1D88;
const KEY_ON_HI: u32 = 0x1F80_1D8A;
const KEY_OFF_LO: u32 = 0x1F80_1D8C;
const KEY_OFF_HI: u32 = 0x1F80_1D8E;
const PITCH_MOD_LO: u32 = 0x1F80_1D90;
const PITCH_MOD_HI: u32 = 0x1F80_1D92;
/// ENDX: one sticky bit per voice, set when that voice decodes a block with
/// the END flag. Write to clear.
const ENDX_LO: u32 = 0x1F80_1D9C;
const ENDX_HI: u32 = 0x1F80_1D9E;
const NOISE_LO: u32 = 0x1F80_1D94;
const NOISE_HI: u32 = 0x1F80_1D96;
const REVERB_ENABLE_LO: u32 = 0x1F80_1D98;
const REVERB_ENABLE_HI: u32 = 0x1F80_1D9A;
const TRANSFER_ADDR: u32 = 0x1F80_1DA6;
const REVERB_WORK_BASE: u32 = 0x1F80_1DA2;
const TRANSFER_DATA: u32 = 0x1F80_1DA8;
const TRANSFER_CTRL: u32 = 0x1F80_1DAC;
const CD_VOL_LEFT: u32 = 0x1F80_1DB0;
const CD_VOL_RIGHT: u32 = 0x1F80_1DB2;
const SPUCNT_CD_AUDIO_ENABLE: u16 = 1 << 0;

// ======================================================================
// Initialisation
// ======================================================================

/// Reset the SPU to a sane playable state.
///
/// Sets:
/// - SPUCNT: enable, unmute, default reverb off, CD audio off
/// - Main volume: max
/// - Every voice: silenced (volume 0, ADSR release fire, key-off)
/// - Reverb input and wet-output depth, pitch-mod, and noise: all disabled
/// - Transfer mode: 16-bit PIO (the upload path we expose)
///
/// Call once at boot before any voice operations.
pub fn init() {
    // Silence everything immediately -- key_off on all 24 voices
    // before we touch any other state, so nothing glitches audibly
    // on cold boot.
    write_reg16(KEY_OFF_LO, 0xFFFF);
    write_reg16(KEY_OFF_HI, 0x00FF);

    // Zero every voice register block.
    for v in 0..24 {
        let base = SPU_BASE + (v as u32) * VOICE_STRIDE;
        write_reg16(base + VOICE_VOL_LEFT, 0);
        write_reg16(base + VOICE_VOL_RIGHT, 0);
        write_reg16(base + VOICE_PITCH, 0);
        write_reg16(base + VOICE_START_ADDR, 0);
        write_reg16(base + VOICE_ADSR_LO, 0);
        write_reg16(base + VOICE_ADSR_HI, 0);
    }

    // Disable reverb, noise, pitch modulation on all voices.
    write_reg16(REVERB_ENABLE_LO, 0);
    write_reg16(REVERB_ENABLE_HI, 0);
    // Clearing SPUCNT's reverb-master bit stops feedback writes but does not
    // stop the hardware read/APF/output path. The retail BIOS leaves a live
    // preset (including non-zero vLOUT and mBASE) behind; a later sample-bank
    // DMA can overwrite that work area and become audible even with EON=0.
    // PA5 on silicon measured 56.8 dB of suppression from these two writes.
    write_reg16(REVERB_VOL_LEFT, 0);
    write_reg16(REVERB_VOL_RIGHT, 0);
    // Park the reverb work area at the very top of SPU RAM.
    //
    // The work area runs from mBASE upward to the end of RAM, so a BIOS-left
    // base low in memory reserves most of SPU RAM for reverb and anything a
    // game uploads above it shares memory with the reverb engine. Zeroing the
    // volumes above silences reverb's OUTPUT but leaves that claim on memory,
    // and nothing else here moves it.
    write_reg16(REVERB_WORK_BASE, 0xFFFE);
    write_reg16(PITCH_MOD_LO, 0);
    write_reg16(PITCH_MOD_HI, 0);
    write_reg16(NOISE_LO, 0);
    write_reg16(NOISE_HI, 0);

    // Main volume to max -- per-voice volume still controls the mix.
    write_reg16(MAIN_VOL_LEFT, Volume::MAX.0 as u16);
    write_reg16(MAIN_VOL_RIGHT, Volume::MAX.0 as u16);
    write_reg16(CD_VOL_LEFT, CdVolume::SILENCE.0 as u16);
    write_reg16(CD_VOL_RIGHT, CdVolume::SILENCE.0 as u16);

    // SPUCNT: bit 15 = SPU enable, bit 14 = mute OFF (i.e. audible),
    // everything else zero. Writing in that order matches PSX-SPX's
    // recommendation -- enable-then-unmute avoids a click.
    write_reg16(SPUCNT, 0x8000); // enabled, muted
    wait_spu_status(0x0000); // wait for SPUSTAT to stabilise
    write_reg16(SPUCNT, 0xC000); // enabled + unmuted
    wait_spu_status(0x0000);

    // Transfer mode: "Stop" (bit 0..=2 = 0). Games toggle this to
    // Manual/DMA as needed when uploading.
    write_reg16(TRANSFER_CTRL, 0x0004); // normal mode

    upload_adpcm(SILENCE_BLOCK, &SILENCE_BLOCK_BYTES);
}

/// One ADPCM block of silence that loops on itself, for every one-shot's
/// repeat address to point at. See [`SILENCE_BLOCK_BYTES`].
///
/// SPU RAM below 0x1000 belongs to the hardware's own decode buffers, so
/// 0x1000 is the first address a program may use, and everything in this
/// workspace already starts its sample bank at 0x1010. That leaves exactly one
/// block spare, which is all this needs.
pub const SILENCE_BLOCK: SpuAddr = SpuAddr::new(0x1000);

/// Shift/filter 0, flags END|REPEAT, and fourteen bytes of zero nibbles.
///
/// A voice that lands here decodes silence and jumps straight back to the top
/// of the same block, so it stays here until something keys it off.
const SILENCE_BLOCK_BYTES: [u8; 16] = [0x00, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Spin budget for one SPU handshake. Generous next to the delay the
/// hardware actually takes, short enough that a wedged SPU hands control
/// back rather than freezing the frame.
const SPU_HANDSHAKE_SPINS: u32 = 100_000;

/// Poll SPUSTAT's "SPU mode" field (low 6 bits) until it matches
/// `want & 0x3F`.
///
/// This is not optional politeness. Per PSX-SPX, SPUCNT bits 0-5 are NOT
/// applied immediately: after writing SPUCNT you must wait until the low
/// bits of SPUSTAT reflect the new mode. Code that writes the transfer
/// mode and starts moving data on the next instruction is talking to an
/// SPU that has not entered that mode yet.
///
/// Bounded, unlike the first version of this: an unbounded spin on a
/// register the hardware may never update is a hung console.
fn wait_spu_status(want: u16) {
    let mask = 0x3F;
    for _ in 0..SPU_HANDSHAKE_SPINS {
        if (read_reg16(SPUSTAT) & mask) == (want & mask) {
            return;
        }
    }
}

/// Wait for SPUSTAT bit 10, the data-transfer busy flag, to clear.
///
/// The DMA channel reporting "done" only means the CPU side finished
/// handing bytes over; the SPU is still draining its own FIFO into sound
/// RAM. Changing the transfer mode before this clears cuts the tail of
/// the upload off.
fn wait_transfer_idle() {
    for _ in 0..SPU_HANDSHAKE_SPINS {
        if read_reg16(SPUSTAT) & 0x0400 == 0 {
            return;
        }
    }
}

// ======================================================================
// Typed primitives
// ======================================================================

/// A 16-bit signed SPU voice/main volume.
///
/// Static voice/main volume uses `-0x4000..=0x3FFF` as its practical
/// linear range. Positive magnitudes are the normal "louder" direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Volume(pub i16);

impl Volume {
    /// Silence.
    pub const SILENCE: Self = Self(0);
    /// Maximum linear-positive volume (0x3FFF = +1.0).
    pub const MAX: Self = Self(0x3FFF);
    /// Half-scale convenience (0x2000 ≈ 0.5).
    pub const HALF: Self = Self(0x2000);

    /// Build from a normalized float-ish 0.0..=1.0 value without
    /// actually using floats (since we're `no_std`, no FPU). `num`
    /// and `den` are integer -- `Volume::linear(3, 4)` is 0.75.
    pub const fn linear(num: u16, den: u16) -> Self {
        let v = ((Self::MAX.0 as u32) * (num as u32)) / (den as u32);
        Self(v as i16)
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::SILENCE
    }
}

/// CD-DA / XA input volume.
///
/// CD input volumes are plain signed Q15 values on hardware, so full
/// positive gain is `0x7FFF`. Use this instead of [`Volume`], whose
/// `0x3FFF` maximum is for voice/main registers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct CdVolume(pub i16);

impl CdVolume {
    /// Silence.
    pub const SILENCE: Self = Self(0);
    /// Maximum positive CD input gain.
    pub const MAX: Self = Self(0x7FFF);
    /// Half-scale CD input gain.
    pub const HALF: Self = Self(0x4000);

    /// Build from a normalized `0.0..=1.0` ratio without floats.
    pub const fn linear(num: u16, den: u16) -> Self {
        let v = ((Self::MAX.0 as u32) * (num as u32)) / (den as u32);
        Self(v as i16)
    }
}

impl Default for CdVolume {
    fn default() -> Self {
        Self::SILENCE
    }
}

/// Q5.12 pitch / sample-rate multiplier. `0x1000` = play at the
/// sample's recorded rate. Halving = one octave down, doubling =
/// one octave up.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Pitch(u16);

impl Pitch {
    /// `0x1000` -- play at the sample's native 44100 Hz rate.
    pub const UNITY: Self = Self(0x1000);
    /// `0x0800` -- one octave below recorded rate.
    pub const OCTAVE_DOWN: Self = Self(0x0800);
    /// `0x2000` -- one octave above recorded rate.
    pub const OCTAVE_UP: Self = Self(0x2000);
    /// Hardware cap (14-bit field). Higher values clamp.
    pub const MAX: Self = Self(0x3FFF);

    /// Raw constructor. Values above `0x3FFF` clamp on hardware.
    pub const fn raw(v: u16) -> Self {
        Self(v)
    }

    /// The raw register value, as [`Pitch::raw`] took it. Callers scaling a
    /// duration by pitch need the number back: at half unity a sample lasts
    /// twice as long, and a cutoff that ignores that clips the sound.
    pub const fn raw_value(self) -> u16 {
        self.0
    }

    /// Pitch that makes a native-rate sine loop play at `hz_num /
    /// hz_den` where the loop's natural frequency is `base_hz`.
    /// Integer-only so `no_std` users can compute at compile time.
    pub const fn for_frequency(target_hz: u32, base_hz: u32) -> Self {
        let raw = ((0x1000u32) * target_hz) / base_hz;
        if raw > 0x3FFF {
            Self(0x3FFF)
        } else {
            Self(raw as u16)
        }
    }

    /// Pitch that plays a decoded sample stream at its declared
    /// source rate. `44100` maps to [`Pitch::UNITY`].
    pub const fn for_sample_rate(sample_rate_hz: u32) -> Self {
        if sample_rate_hz == 0 {
            return Self::UNITY;
        }
        let raw = ((0x1000u32) * sample_rate_hz + 22_050) / 44_100;
        if raw > 0x3FFF {
            Self(0x3FFF)
        } else if raw == 0 {
            Self(1)
        } else {
            Self(raw as u16)
        }
    }

    /// Underlying 14-bit value.
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

/// An 8-byte-aligned SPU RAM address.
///
/// SPU voice-start / loop / transfer registers all hold `addr / 8`
/// in their 16-bit field. Construction asserts alignment so the
/// division is lossless and caller bugs don't silently round down.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SpuAddr(u32);

impl SpuAddr {
    /// Build from a byte offset into SPU RAM. Panics if `addr`
    /// isn't a multiple of 8.
    #[allow(clippy::manual_is_multiple_of)]
    pub const fn new(addr: u32) -> Self {
        assert!(addr % 8 == 0, "SpuAddr: addr must be multiple of 8");
        assert!(addr < 512 * 1024, "SpuAddr: past end of 512 KiB SPU RAM",);
        Self(addr)
    }

    /// The byte offset this address represents.
    pub const fn byte_offset(self) -> u32 {
        self.0
    }

    /// The 16-bit field value the SPU registers actually want
    /// (`addr / 8`).
    pub const fn reg_field(self) -> u16 {
        (self.0 / 8) as u16
    }
}

/// ADSR envelope descriptor. Builds the pair of 16-bit envelope
/// registers the SPU needs.
///
/// Hardware bit-fields (PSX-SPX § "Voice 0..23 ADSR Register"):
///
/// **ADSR Lower (u16):**
/// ```text
///   bits 0..=3  : sustain level (0..15, sets target after decay)
///   bits 4..=7  : decay shift (0..15)
///   bits 8..=14 : attack shift (0..127)
///   bit  15     : attack mode (0 = linear, 1 = exponential)
/// ```
///
/// **ADSR Upper (u16):**
/// ```text
///   bits 0..=4  : release shift (0..31)
///   bit  5      : release mode (0 = linear, 1 = exponential)
///   bits 6..=12 : sustain shift (0..127)
///   bit  13     : reserved
///   bit  14     : sustain direction (0 = increase, 1 = decrease)
///   bit  15     : sustain mode (0 = linear, 1 = exponential)
/// ```
///
/// You rarely want to set all of these by hand; use
/// [`Adsr::default_tone`] for a generic instrument-like preset or
/// [`Adsr::percussive`] for a snappy one-shot.
#[derive(Copy, Clone, Debug)]
pub struct Adsr {
    /// Pre-packed 16-bit ADSR lower word.
    pub lower: u16,
    /// Pre-packed 16-bit ADSR upper word.
    pub upper: u16,
}

impl Adsr {
    /// Generic sustained-tone envelope: instant attack to full, hold at
    /// max sustain until [`Voice::key_off`], then an exponential release
    /// (~100ms). The right default for melodies and held notes.
    ///
    /// The original version packed "attack shift 0x7F" into ADSR1's
    /// 5-bit attack-shift field (bits 14-10); the overflow encoded
    /// attack shift 31 + step 3 -- the slowest envelope the hardware
    /// can express, so the voice never rose above the noise floor.
    /// Layout per PSX-SPX: ADSR1 = [15 atk mode][14-10 atk shift]
    /// [9-8 atk step][7-4 decay shift][3-0 sustain level]; ADSR2 =
    /// [15 sus mode][14 sus dir][12-8 sus shift][7-6 sus step]
    /// [5 rel mode][4-0 rel shift].
    pub const fn default_tone() -> Self {
        Self {
            // Linear attack, shift 0, step 0 (instant); no decay;
            // sustain level 0xF (max).
            lower: 0x000F,
            // Sustain holds (linear increase, capped at max);
            // exponential release, shift 7.
            upper: 0x0027,
        }
    }

    /// Percussive one-shot -- blips, UI clicks, hit SFX. Instant attack
    /// to full, then a snappy exponential fade in the sustain phase
    /// (~150ms), so the voice silences itself without a key_off.
    pub const fn percussive() -> Self {
        Self {
            // Instant attack, no decay, sustain level max.
            lower: 0x000F,
            // Sustain: exponential decrease, shift 6 (the self-fade);
            // exponential release, shift 5.
            upper: 0xC625,
        }
    }

    /// Full-level envelope for sampled playback that a caller will end
    /// itself (looped holds, streams). NOT for fire-and-forget one-shots:
    /// on real hardware the ADPCM END+mute terminator does not silence
    /// the voice, it drops it into RELEASE at this envelope's release
    /// rate -- which is the slowest the hardware encodes -- while the
    /// voice loops from the repeat address. A one-shot keyed under this
    /// envelope therefore repeats at full volume indefinitely, and
    /// key_off does not audibly help. Measured by hardware-tests SB1 on
    /// console (2026-08-02): envelope 7FFF at 4.7 s with ENDX long set,
    /// 3.7 s after key_off. The emulator zeroes the envelope at the
    /// terminator instead, so only silicon shows it. Use
    /// [`Adsr::percussive`] for one-shots; SB1 measured it silent within
    /// two frames on the same console.
    pub const fn sample() -> Self {
        Self {
            lower: 0x000F,
            upper: 0x001F,
        }
    }

    /// All-zero ADSR.
    ///
    /// This does NOT hold the voice at key-on volume, despite being the
    /// obvious "no envelope" choice: zero means sustain level 0, so on real
    /// hardware the envelope decays to silence shortly after key-on. Verified
    /// on console, where a held tone faded out after ~3 seconds. Use
    /// [`Adsr::sample`] for a level that holds.
    pub const fn passthrough() -> Self {
        Self { lower: 0, upper: 0 }
    }
}

// ======================================================================
// Voice handle
// ======================================================================

/// A typed voice index in `0..24`. Construct via the `V0`..`V23`
/// constants or [`Voice::new`]. All per-voice operations are
/// methods on this type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Voice(u8);

impl Voice {
    /// Voice index 0.
    pub const V0: Self = Self(0);
    /// Voice index 1.
    pub const V1: Self = Self(1);
    /// Voice index 2.
    pub const V2: Self = Self(2);
    /// Voice index 3.
    pub const V3: Self = Self(3);
    /// Voice index 4.
    pub const V4: Self = Self(4);
    /// Voice index 5.
    pub const V5: Self = Self(5);
    /// Voice index 6.
    pub const V6: Self = Self(6);
    /// Voice index 7.
    pub const V7: Self = Self(7);
    // V8..V23 follow the same pattern; keep them collapsed until a
    // game actually needs 9+ voices in flight. `Voice::new(N)` is
    // always available as the escape hatch.

    /// Build a voice by index. Panics (const-asserts) if `n >= 24`.
    pub const fn new(n: u8) -> Self {
        assert!(n < 24, "Voice: index must be < 24");
        Self(n)
    }

    /// The 0-based voice index this handle points at.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Bitmask this voice occupies in the 24-bit key-on / key-off
    /// register pair. `Voice::V0.mask() == 0b1`.
    pub const fn mask(self) -> u32 {
        1u32 << (self.0 as u32)
    }

    /// Base MMIO address of this voice's 16-byte register block.
    #[inline]
    const fn reg_base(self) -> u32 {
        SPU_BASE + (self.0 as u32) * VOICE_STRIDE
    }

    /// Set per-voice stereo volume. The main-output mix is still
    /// modulated by [`set_main_volume`], but each voice can have
    /// its own level.
    pub fn set_volume(self, left: Volume, right: Volume) {
        write_reg16(self.reg_base() + VOICE_VOL_LEFT, left.0 as u16);
        write_reg16(self.reg_base() + VOICE_VOL_RIGHT, right.0 as u16);
    }

    /// Set the voice's sample-rate pitch (Q5.12). [`Pitch::UNITY`]
    /// = native 44100 Hz.
    pub fn set_pitch(self, pitch: Pitch) {
        write_reg16(self.reg_base() + VOICE_PITCH, pitch.as_u16());
    }

    /// Point the voice at the ADPCM sample starting at `addr`.
    /// Voice will begin playing from this address at the next key-on.
    pub fn set_start_addr(self, addr: SpuAddr) {
        write_reg16(self.reg_base() + VOICE_START_ADDR, addr.reg_field());
    }

    /// Set the voice's loop (repeat) address: where playback jumps when a
    /// sample block with the loop-end flag finishes. The hardware itself
    /// rewrites this register when a block with the loop-start flag passes,
    /// so set it AFTER key-on to override; chaining two buffers' loop
    /// addresses is how streamed audio ping-pongs without a key-off.
    pub fn set_loop_addr(self, addr: SpuAddr) {
        write_reg16(self.reg_base() + VOICE_REPEAT_ADDR, addr.reg_field());
    }

    /// Install ADSR envelope parameters on this voice.
    pub fn set_adsr(self, adsr: Adsr) {
        write_reg16(self.reg_base() + VOICE_ADSR_LO, adsr.lower);
        write_reg16(self.reg_base() + VOICE_ADSR_HI, adsr.upper);
    }

    /// Configure this voice to play a sample already uploaded at `addr`.
    pub fn configure_sample(self, addr: SpuAddr, sample_rate_hz: u32, volume: Volume, adsr: Adsr) {
        self.set_volume(volume, volume);
        self.set_pitch(Pitch::for_sample_rate(sample_rate_hz));
        self.set_start_addr(addr);
        // Where silicon jumps when the sample's last block raises END. The
        // hardware latches this register itself, but only from a block
        // carrying the loop-start flag, and a one-shot has none: every psau in
        // this workspace is 0x00 on the first block and 0x01 on the last. So
        // without this write the register simply keeps whatever a previous
        // sound left in it, and the voice runs off the end of its own data
        // into whichever sample happens to sit at that stale address.
        //
        // The 2026-08-03 SB2 capture read it back on silicon: 0x034C for
        // thirteen of fifteen segments regardless of what was keyed, against a
        // start of 0x0202. The one segment that wrote the register explicitly
        // read back correct, which is what says this write takes.
        //
        // Audibly, it was a blip after the launcher's browse sound (jump at
        // 0x1010 running into pickup_coin at 0x28B0) and after VoXide's
        // footstep. A caller that wants a genuine loop sets its own address
        // after this returns.
        self.set_loop_addr(SILENCE_BLOCK);
        self.set_adsr(adsr);
    }

    /// Trigger the voices whose bits are set in `mask`. The SPU
    /// begins playing each voice from its configured start address,
    /// applying its ADSR attack phase.
    ///
    /// Example: `Voice::key_on(Voice::V0.mask() | Voice::V3.mask())`.
    pub fn key_on(mask: u32) {
        write_reg16(KEY_ON_LO, mask as u16);
        write_reg16(KEY_ON_HI, (mask >> 16) as u16);
    }

    /// Which voices have decoded a block carrying the END flag since ENDX was
    /// last cleared, one sticky bit each.
    ///
    /// This is the only direct evidence that a one-shot reached its own
    /// terminator. Everything else about termination has to be inferred: the
    /// repeat address says where the hardware *would* jump, not whether it
    /// did, and the 2026-08-03 SB2 capture read that register back as correct
    /// while voices were audibly running into the next sample anyway.
    pub fn voices_ended() -> u32 {
        read_reg16(ENDX_LO) as u32 | ((read_reg16(ENDX_HI) as u32) << 16)
    }

    /// Clear the sticky END flags for the voices in `mask`, so the next
    /// [`Voice::voices_ended`] reports only what happened after this call.
    pub fn clear_ended(mask: u32) {
        write_reg16(ENDX_LO, mask as u16);
        write_reg16(ENDX_HI, (mask >> 16) as u16);
    }

    /// Stop the voices whose bits are set in `mask` -- fires the
    /// release phase of the ADSR.
    pub fn key_off(mask: u32) {
        write_reg16(KEY_OFF_LO, mask as u16);
        write_reg16(KEY_OFF_HI, (mask >> 16) as u16);
    }

    /// Route the voices whose bits are set in `mask` to the noise generator
    /// instead of their ADPCM data (the classic PSX percussion/wind source);
    /// voices outside `mask` play normally. [`init`] clears the mask. Pitch
    /// comes from the shared noise clock, [`set_noise_clock`], not the
    /// voice's pitch register.
    pub fn set_noise_mask(mask: u32) {
        write_reg16(NOISE_LO, mask as u16);
        write_reg16(NOISE_HI, (mask >> 16) as u16);
    }
}

// ======================================================================
// Global controls
// ======================================================================

/// Set the main L/R output volume that every voice mixes through.
pub fn set_main_volume(left: Volume, right: Volume) {
    write_reg16(MAIN_VOL_LEFT, left.0 as u16);
    write_reg16(MAIN_VOL_RIGHT, right.0 as u16);
}

/// Set the SPU CD input volume.
///
/// CD-DA and XA samples first leave the CD-ROM controller, then enter
/// the SPU's CD input where these gains are applied before main mix.
pub fn set_cd_volume(left: CdVolume, right: CdVolume) {
    write_reg16(CD_VOL_LEFT, left.0 as u16);
    write_reg16(CD_VOL_RIGHT, right.0 as u16);
}

/// Set the shared noise-generator clock, SPUCNT bits 8..=13: `shift`
/// (0..=15) halves the frequency per step, 0 = highest pitch; `step`
/// (0..=3) selects the +4..+7 fine increment. Applies to every voice
/// routed to noise via [`Voice::set_noise_mask`].
pub fn set_noise_clock(shift: u8, step: u8) {
    const NOISE_BITS: u16 = 0x3F00; // bits 8..=13
    let field = (((shift as u16) & 0xF) << 10) | (((step as u16) & 0x3) << 8);
    let control = read_reg16(SPUCNT) & !NOISE_BITS;
    write_reg16(SPUCNT, control | field);
}

/// Enable or disable routing CD-DA/XA input into the SPU mixer.
pub fn enable_cd_audio(enabled: bool) {
    let mut control = read_reg16(SPUCNT);
    if enabled {
        control |= SPUCNT_CD_AUDIO_ENABLE;
    } else {
        control &= !SPUCNT_CD_AUDIO_ENABLE;
    }
    write_reg16(SPUCNT, control);
}

// ======================================================================
// ADPCM upload (PIO path)
// ======================================================================

/// Upload ADPCM sample bytes to SPU RAM via the manual-transfer
/// (PIO) path. Slow -- one halfword per write -- but simple and
/// doesn't need DMA setup. Games that upload many megabytes of
/// samples at boot use DMA; for per-frame SFX uploads of a few
/// KB, this is fine.
///
/// `bytes` must be a multiple of 2 (one halfword per two bytes).
/// `dest` must be 8-byte-aligned (the SPU voice-start register's
/// resolution).
///
/// Process (PSX-SPX § "SPU Data Transfer"):
/// 1. Write transfer-control = 0 (reset).
/// 2. Write target address register.
/// 3. Write transfer-control = 4 (manual).
/// 4. Push halfword data through 0x1F80_1DA8.
/// 5. Wait for the transfer to drain (SPUSTAT bit 7 = transfer busy).
pub fn upload_adpcm(dest: SpuAddr, bytes: &[u8]) {
    assert!(
        bytes.len().is_multiple_of(2),
        "upload_adpcm: byte slice must be a multiple of 2",
    );
    // The silent block every one-shot's repeat address points at lives at
    // 0x1000, so a bank laid over it takes away the only thing stopping a
    // finished voice wandering into other samples -- and does it silently,
    // since the symptom is a stray blip rather than a crash. Celeste's PICO-8
    // waveform bank starts at exactly 0x1000, which is how this was found.
    // Banks start at 0x1010.
    assert!(
        dest.byte_offset() >= SILENCE_BLOCK.byte_offset() + SILENCE_BLOCK_BYTES.len() as u32
            || core::ptr::eq(bytes.as_ptr(), SILENCE_BLOCK_BYTES.as_ptr()),
        "upload_adpcm: destination overlaps the SPU silence block at 0x1000",
    );
    // Prefer DMA (channel 4): fast, and the only reliable path for uploads
    // larger than the 32-halfword transfer FIFO. Falls back to PIO when the
    // source is not word-aligned / a whole number of 32-bit words.
    if !upload_adpcm_dma(dest, bytes) {
        upload_adpcm_pio(dest, bytes);
    }
}

/// DMA upload (channel 4, RAM -> SPU RAM), block-sync. Returns `false` (so the
/// caller falls back to PIO) when `bytes` is not 4-byte aligned or not a whole
/// number of 32-bit words -- the DMA controller is word-addressed.
fn upload_adpcm_dma(dest: SpuAddr, bytes: &[u8]) -> bool {
    let src = bytes.as_ptr() as u32;
    if !src.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
        return false;
    }
    let words = (bytes.len() / 4) as u32;
    // Largest block size (in words) that divides the count; the SPU transfer
    // FIFO is 16 words, so cap there. ADPCM is whole 16-byte (4-word) blocks,
    // so 4 always divides.
    let block_size = if words.is_multiple_of(16) {
        16
    } else if words.is_multiple_of(8) {
        8
    } else if words.is_multiple_of(4) {
        4
    } else if words.is_multiple_of(2) {
        2
    } else {
        1
    };
    let block_count = words / block_size;
    if block_count > 0xFFFF {
        return false; // larger than one block-sync transfer can describe
    }

    // Stop any in-flight transfer, and WAIT for the SPU to really be
    // stopped before touching anything else. Going through Stop is what
    // makes the wait below meaningful: SPUSTAT only tells us the mode
    // changed if it actually changed.
    let spucnt = read_reg16(SPUCNT) & !0x0030;
    write_reg16(SPUCNT, spucnt);
    wait_spu_status(spucnt);
    write_reg16(TRANSFER_CTRL, 0x0004);
    write_reg16(TRANSFER_ADDR, dest.reg_field());
    // SPUCNT transfer mode = DMA Write (bits 5..4 = 10), then wait for
    // the SPU to enter it. Without this the DMA can deliver the whole
    // payload to an SPU still in Stop mode -- and the smaller the
    // payload, the more of it is lost, which is exactly the size
    // dependence the console showed: kilobyte banks mostly arrived while
    // a 32-byte table arrived not at all.
    write_reg16(SPUCNT, spucnt | 0x0020);
    wait_spu_status(spucnt | 0x0020);

    // Channel 4: main RAM -> SPU, block-sync, forward, start; block until done.
    dma::enable_channel(Channel::Spu);
    dma::set_madr(Channel::Spu, src);
    dma::set_bcr_block(Channel::Spu, block_size as u16, block_count as u16);
    dma::set_chcr(
        Channel::Spu,
        dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START,
    );
    if !dma::wait_done(Channel::Spu, dma::DEFAULT_DMA_SPINS) {
        dma::abort(Channel::Spu);
    }
    // The channel is done handing bytes over; the SPU is still writing
    // them into sound RAM.
    wait_transfer_idle();

    // Return to Stop transfer-mode.
    write_reg16(SPUCNT, spucnt);
    true
}

/// PIO upload (manual FIFO writes) -- small or non-word-aligned uploads. Sets
/// SPUCNT Manual-Write mode (bits 5..4 = 01) around the writes, the step the
/// original path was missing (it only poked TRANSFER_CTRL), which silently
/// no-op'd the upload on hardware and on FIFO-accurate emulators.
fn upload_adpcm_pio(dest: SpuAddr, bytes: &[u8]) {
    let spucnt = read_reg16(SPUCNT) & !0x0030;
    write_reg16(SPUCNT, spucnt);
    wait_spu_status(spucnt);
    write_reg16(TRANSFER_CTRL, 0x0004);
    write_reg16(TRANSFER_ADDR, dest.reg_field());
    // Manual Write (bits 5..4 = 01), and wait for the SPU to be in it
    // before pushing a single halfword. Same reason as the DMA path.
    write_reg16(SPUCNT, spucnt | 0x0010);
    wait_spu_status(spucnt | 0x0010);

    let mut i = 0;
    while i + 1 < bytes.len() {
        let lo = bytes[i] as u16;
        let hi = bytes[i + 1] as u16;
        write_reg16(TRANSFER_DATA, lo | (hi << 8));
        i += 2;
    }

    wait_transfer_idle();
    write_reg16(SPUCNT, spucnt);
    // Leave the transfer type NORMAL, not 0. PSX-SPX is explicit that
    // 1F801DACh "should be 0004h"; parking it at 0 selects another
    // transfer type, and on silicon that poisons everything that touches
    // sample RAM afterwards -- voices key on and their envelope collapses
    // straight back to zero, so every sampled sound goes silent while
    // noise-mode voices, which never read SPU RAM, keep playing. Measured
    // by hardware-tests SB2 on console (2026-08-02): after a probe that
    // left this at 0, its own tone ladder was silent and a following SB1
    // run was silent too, both fine on the emulator, which ignores this
    // register entirely.
    write_reg16(TRANSFER_CTRL, 0x0004);
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_silence_block_ends_and_repeats_onto_itself() {
        // Without both flags the voice either runs on past this block or
        // stops here by luck rather than by contract.
        assert_eq!(SILENCE_BLOCK_BYTES[1] & 0x01, 0x01, "END");
        assert_eq!(SILENCE_BLOCK_BYTES[1] & 0x02, 0x02, "REPEAT");
        assert!(
            SILENCE_BLOCK_BYTES[2..].iter().all(|&b| b == 0),
            "the block has to decode to silence",
        );
    }

    #[test]
    fn the_silence_block_owns_the_first_usable_block_of_spu_ram() {
        // SPU RAM below 0x1000 is the hardware's own decode area, so 0x1000 is
        // the first address a program may use and this claims it. A bank laid
        // over it takes away the only thing stopping a finished voice
        // wandering, which is why upload_adpcm refuses the overlap rather than
        // trusting every game to start at 0x1010.
        assert_eq!(SILENCE_BLOCK.byte_offset(), 0x1000);
        assert_eq!(SILENCE_BLOCK_BYTES.len(), 16);
    }

    #[test]
    fn volume_linear_scales() {
        assert_eq!(Volume::linear(0, 1), Volume::SILENCE);
        assert_eq!(Volume::linear(1, 1), Volume::MAX);
        // 0.5 → 0x1FFF (rounded down from 0x3FFF / 2).
        let half = Volume::linear(1, 2);
        assert_eq!(half.0, 0x1FFF);
    }

    #[test]
    fn cd_volume_uses_q15_full_scale() {
        assert_eq!(CdVolume::linear(0, 1), CdVolume::SILENCE);
        assert_eq!(CdVolume::linear(1, 1), CdVolume::MAX);
        assert_eq!(CdVolume::HALF.0, 0x4000);
    }

    #[test]
    fn pitch_for_frequency() {
        // Play a 440 Hz native sample at 880 Hz → pitch 0x2000.
        let p = Pitch::for_frequency(880, 440);
        assert_eq!(p.as_u16(), 0x2000);
        // Play at native rate.
        let p = Pitch::for_frequency(440, 440);
        assert_eq!(p, Pitch::UNITY);
        // Very high multiplier clamps to MAX.
        let p = Pitch::for_frequency(100_000, 440);
        assert_eq!(p, Pitch::MAX);
    }

    #[test]
    fn pitch_for_sample_rate_maps_to_spu_step() {
        assert_eq!(Pitch::for_sample_rate(44_100), Pitch::UNITY);
        assert_eq!(Pitch::for_sample_rate(22_050).as_u16(), 0x0800);
        assert_eq!(Pitch::for_sample_rate(88_200).as_u16(), 0x2000);
    }

    #[test]
    fn spu_addr_reg_field_divides_by_8() {
        assert_eq!(SpuAddr::new(0x1000).reg_field(), 0x0200);
        assert_eq!(SpuAddr::new(0x1008).reg_field(), 0x0201);
    }

    #[test]
    #[should_panic = "multiple of 8"]
    fn spu_addr_rejects_misalignment() {
        let _ = SpuAddr::new(0x1004);
    }

    #[test]
    fn voice_mask_correct() {
        assert_eq!(Voice::V0.mask(), 0x01);
        assert_eq!(Voice::V3.mask(), 0x08);
        assert_eq!(Voice::new(23).mask(), 0x0080_0000);
    }

    #[test]
    #[should_panic = "index must be < 24"]
    fn voice_new_rejects_out_of_range() {
        let _ = Voice::new(24);
    }
}
