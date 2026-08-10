//! One-shot sample playback on the SPU.
//!
//! Four programs on the PSoXide demo disc grew their own copy of this: the
//! launcher, VoXide, NitroXide and the Celeste collection. Three of them are
//! not engine applications, so the helpers in `psx-engine` were out of reach
//! and each rewrote the same three things. That mattered more than the
//! duplication itself, because what they were rewriting is the part emulators
//! get wrong:
//!
//! * A one-shot's repeat address has to be written by hand. Silicon latches it
//!   only from a block carrying the loop-start flag, and a one-shot has none,
//!   so it otherwise keeps whatever the previous sound left there and the
//!   voice ends up playing an unrelated sample. [`psx_spu::Voice::
//!   configure_sample`] does this now; going through it is the point.
//! * A one-shot does not stop itself. END drops the voice into RELEASE at the
//!   ADSR's release rate rather than muting it, so a slow release leaves the
//!   voice running audibly past its own data. A fast release hides that, which
//!   is why the bug survived so long: the Celeste collection is inaudibly
//!   wrong rather than right. The honest fix is to silence the voice on a
//!   clock, which is what [`Player::tick`] does.
//! * Voices have to be shared out, or a new sound cuts off the one before it.
//!
//! ```ignore
//! let mut bank = Bank::new(SpuAddr::new(0x1010));
//! let blip = bank.upload(include_bytes!("blip.psau"));
//! let mut player: Player<3> = Player::new([Voice::V0, Voice::V1, Voice::V2], 60);
//!
//! // once a frame
//! player.tick(tick);
//! if pressed { player.play(&OneShot::new(blip, Volume::HALF), tick); }
//! ```

#![no_std]

use psx_asset::Audio;
use psx_spu::{Adsr, Pitch, SpuAddr, Voice, Volume};

/// Decoded samples in one ADPCM block. Fourteen data bytes, two nibbles each.
const SAMPLES_PER_BLOCK: u32 = 28;

/// Bytes in one ADPCM block: a shift/filter byte, a flags byte, and fourteen
/// of data.
const BLOCK_BYTES: u32 = 16;

/// One ADPCM block of silence that loops onto itself, written after every
/// uploaded sample so a voice running past its own END lands on nothing.
///
/// Shift/filter 0, flags `0x07` = LOOP-START | REPEAT | END, then fourteen
/// bytes of zero nibbles. LOOP-START is the bit that matters: the hardware
/// latches the repeat address off this block while decoding it, which does not
/// depend on anything written before key-on.
/// Self-looping silent ADPCM block appended after each packed one-shot.
///
/// Cookers that build a bank for later bulk upload should append this after
/// every sample. This gives silicon the same loop-start marker as [`Bank`]
/// without duplicating the hardware-specific byte sequence outside the SDK.
pub const PARKING_TAIL: [u8; BLOCK_BYTES as usize] =
    [0x00, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Ticks of margin added past a sample's own length before its voice is
/// silenced, so the cutoff never clips a sound that is still sounding.
///
/// This margin is exactly the window the browse blip was audibly wandering in
/// before SAMPLE_TAIL existed: 0.250 s of sample against a 0.283 s cutoff left
/// 33 ms in which the voice played the next sample in the bank. The margin is
/// safe to keep now that the sample parks itself.
///
/// The length is computed from the block count, which rounds up to a whole
/// block, and the key-on itself lands somewhere inside the current tick.
const TAIL_TICKS: u32 = 2;

/// A sample resident in SPU RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    addr: SpuAddr,
    rate_hz: u32,
    frames: u32,
}

impl Sample {
    /// Describe a sample already resident in SPU RAM, for banks uploaded by
    /// something other than [`Bank`] -- VoXide streams its straight off the
    /// disc into the SPU, never holding it in main RAM.
    ///
    /// `blocks` is the sample's ADPCM block count, or 0 when the bank format
    /// does not record one. Length is what a cutoff is computed from, so a
    /// sample without it gets no cutoff and falls back to the envelope: see
    /// [`Sample::ticks_at`].
    pub const fn resident(addr: SpuAddr, rate_hz: u32, blocks: u32) -> Self {
        Self {
            addr,
            rate_hz,
            frames: blocks,
        }
    }

    /// Where the sample starts in SPU RAM.
    pub const fn addr(&self) -> SpuAddr {
        self.addr
    }

    /// The rate the sample was cooked at.
    pub const fn sample_rate_hz(&self) -> u32 {
        self.rate_hz
    }

    /// Decoded length in samples.
    pub const fn len_samples(&self) -> u32 {
        self.frames * SAMPLES_PER_BLOCK
    }

    /// How long this sample runs, in ticks of a `ticks_hz` clock, played at
    /// its own rate, with [`TAIL_TICKS`] of margin. `None` when the block
    /// count is unknown, which means no cutoff can be computed and the
    /// envelope has to finish the sound instead.
    ///
    /// Pitch-shifted playback stretches this: see [`OneShot::with_pitch`].
    pub const fn ticks_at(&self, ticks_hz: u32) -> Option<u32> {
        if self.frames == 0 {
            return None;
        }
        let rate = if self.rate_hz == 0 { 1 } else { self.rate_hz };
        Some(self.len_samples() * ticks_hz / rate + TAIL_TICKS)
    }
}

/// A sequential SPU RAM allocator: uploads cooked `.psau` samples one after
/// another and hands back where each landed.
///
/// Start it at or above [`psx_spu::SILENCE_BLOCK`] plus one block. `psx-spu`
/// keeps the first usable block of SPU RAM for the silence every finished
/// one-shot parks on, and [`psx_spu::upload_adpcm`] rejects a bank laid over
/// it rather than letting the collision be silent.
pub struct Bank {
    next: SpuAddr,
}

impl Bank {
    /// Start a bank at `base`.
    pub const fn new(base: SpuAddr) -> Self {
        Self { next: base }
    }

    /// Where the next upload will land, and so the first address free after
    /// everything uploaded so far.
    pub const fn next_addr(&self) -> SpuAddr {
        self.next
    }

    /// Upload one cooked `.psau` sample.
    ///
    /// # Panics
    /// If `psau` is not a valid cooked sample, matching the rest of the SDK's
    /// asset handling: a bad `include_bytes!` is a build mistake, not a
    /// runtime condition worth threading a Result through a sound effect for.
    pub fn upload(&mut self, psau: &[u8]) -> Sample {
        let audio = Audio::from_bytes(psau).expect("cooked psau sample");
        let adpcm = audio.adpcm_bytes();
        psx_spu::upload_adpcm(self.next, adpcm);
        let sample = Sample {
            addr: self.next,
            rate_hz: audio.sample_rate_hz(),
            frames: adpcm.len() as u32 / BLOCK_BYTES,
        };
        self.next = SpuAddr::new(self.next.byte_offset() + adpcm.len() as u32);
        // A parking block after every sample, not just one shared block that
        // the repeat register points at.
        //
        // Pointing the register at psx_spu::SILENCE_BLOCK is not enough on
        // silicon. The 2026-08-04 launcher capture caught the browse voice
        // running off the end of jump at 0x1010 straight into pickup_coin at
        // 0x28B0: the stray 40 ms burst matched the coin's opening spectrum on
        // all five peaks, 2800/2825/2850/8450/8475 Hz. SB2 had read the repeat
        // register back as 0x0200, which is that shared block, so the register
        // was right and the voice went past it anyway.
        //
        // VoXide is the counter-example that says what actually works: its
        // cooker appends a self-looping silent block to every sample and it has
        // never had the artefact. The difference is the loop-start bit. This
        // block carries END, REPEAT and LOOP-START, so the hardware latches the
        // repeat address to the block itself as it decodes it, rather than
        // relying on a value written before key-on.
        psx_spu::upload_adpcm(self.next, &PARKING_TAIL);
        self.next = SpuAddr::new(self.next.byte_offset() + BLOCK_BYTES);
        sample
    }
}

/// A sample with the settings it should be played at.
#[derive(Clone, Copy)]
pub struct OneShot {
    sample: Sample,
    volume: Volume,
    adsr: Adsr,
    pitch: Option<Pitch>,
}

impl OneShot {
    /// A one-shot at its own recorded rate, under [`Adsr::default_tone`].
    ///
    /// `default_tone` rather than `percussive`: percussive self-fades in about
    /// 150 ms, which is most of a quarter-second sound. The cutoff, not the
    /// envelope, is what stops this.
    pub const fn new(sample: Sample, volume: Volume) -> Self {
        Self {
            sample,
            volume,
            adsr: Adsr::default_tone(),
            pitch: None,
        }
    }

    /// Play at a different level. For sounds whose loudness depends on what
    /// happened -- how hard a hit landed, a volume setting -- rather than on
    /// which sample it is.
    pub const fn with_volume(mut self, volume: Volume) -> Self {
        self.volume = volume;
        self
    }

    /// Override the envelope.
    pub const fn with_adsr(mut self, adsr: Adsr) -> Self {
        self.adsr = adsr;
        self
    }

    /// Play at an explicit pitch instead of the sample's own rate. `0x1000` is
    /// unity; lower stretches the sound and raises how long it lasts, which
    /// [`OneShot::ticks`] accounts for.
    pub const fn with_pitch(mut self, pitch: Pitch) -> Self {
        self.pitch = Some(pitch);
        self
    }

    /// The sample behind this one-shot.
    pub const fn sample(&self) -> Sample {
        self.sample
    }

    /// How long this will sound, in ticks of a `ticks_hz` clock, with any
    /// pitch override applied. `None` when the sample's length is unknown.
    pub const fn ticks(&self, ticks_hz: u32) -> Option<u32> {
        let Some(base) = self.sample.ticks_at(ticks_hz) else {
            return None;
        };
        match self.pitch {
            // Pitch is Q4.12, so unity is 0x1000: half the pitch is twice the
            // duration. Guard the zero a caller could hand us rather than
            // dividing by it.
            //
            // Clamped, because the hardware clamps. Pitch::raw takes any u16
            // but silicon caps the register at 0x3FFF: the 2026-08-03 SB2
            // capture played 0x3FFF and 0x5000 back at the same 15169 Hz. A
            // cutoff timed off the requested 0x5000 would expect the sound to
            // finish in four fifths of the time it actually takes, and clip it.
            Some(p) => {
                let requested = p.raw_value();
                let cap = Pitch::MAX.raw_value();
                let raw = if requested > cap { cap } else { requested };
                match raw {
                    0 => Some(base),
                    raw => Some(base * 0x1000 / raw as u32),
                }
            }
            None => Some(base),
        }
    }

    /// Point `voice` at this one-shot without starting it.
    ///
    /// Separate from [`OneShot::play`] because a program that keeps one voice
    /// per sound can set it up once at boot and then only key on. A program
    /// sharing voices has to do both together.
    pub fn configure(&self, voice: Voice) {
        voice.configure_sample(
            self.sample.addr,
            self.sample.rate_hz,
            self.volume,
            self.adsr,
        );
        if let Some(pitch) = self.pitch {
            voice.set_pitch(pitch);
        }
    }

    /// Key this one-shot on `voice`.
    ///
    /// Volume is set on every key-on rather than once at upload, because the
    /// cutoff silences a voice by writing volume 0 and `key_on` does not
    /// restore it. Omitting this is how the demo disc's v0.11 pressing ended
    /// up with a browse blip that played exactly once and never again.
    pub fn play(&self, voice: Voice) {
        self.configure(voice);
        Voice::key_on(voice.mask());
    }
}

/// Has `now` reached `deadline`, on a tick counter that wraps?
///
/// Split out so the wrap is testable off-hardware. A plain `now >= deadline`
/// stops working the first time the counter rolls over, which on a 60 Hz clock
/// is a little over two years of uptime, but the comparison costs the same
/// either way.
const fn expired(now: u32, deadline: u32) -> bool {
    now.wrapping_sub(deadline) < u32::MAX / 2
}

/// Plays one-shots across a fixed set of voices, silencing each when its
/// sample has run out.
///
/// `N` voices are driven round-robin by [`Player::play`], or addressed
/// directly by [`Player::play_on`] when a sound wants a voice of its own.
pub struct Player<const N: usize> {
    voices: [Voice; N],
    /// Tick each voice must be silenced on, or `None` when it is idle.
    off_at: [Option<u32>; N],
    next: usize,
    ticks_hz: u32,
}

impl<const N: usize> Player<N> {
    /// Drive `voices` from a clock running at `ticks_hz` (60 for NTSC frames,
    /// 50 for PAL).
    pub const fn new(voices: [Voice; N], ticks_hz: u32) -> Self {
        Self {
            voices,
            off_at: [None; N],
            next: 0,
            ticks_hz,
        }
    }

    /// Silence any voice whose sample has ended. Call once per tick, before
    /// anything that might start a new sound.
    pub fn tick(&mut self, now: u32) {
        for (slot, deadline) in self.off_at.iter_mut().enumerate() {
            if let Some(at) = *deadline {
                if expired(now, at) {
                    self.voices[slot].set_volume(Volume::SILENCE, Volume::SILENCE);
                    *deadline = None;
                }
            }
        }
    }

    /// Fire `shot` on a specific voice slot.
    ///
    /// A shot whose sample has no recorded length gets no cutoff: the voice
    /// runs until its envelope finishes it, which is what every one-shot did
    /// before this crate existed.
    pub fn play_on(&mut self, slot: usize, shot: &OneShot, now: u32) {
        let voice = self.voices[slot];
        shot.play(voice);
        self.off_at[slot] = shot.ticks(self.ticks_hz).map(|t| now.wrapping_add(t));
    }

    /// Fire `shot` on the next voice in the rotation, and return which slot
    /// took it.
    ///
    /// Round-robin rather than picking an idle voice: finding the idle one
    /// costs a scan, and a rotation of a few voices means a new sound rarely
    /// lands on one that is still sounding anyway.
    pub fn play(&mut self, shot: &OneShot, now: u32) -> usize {
        let slot = self.next;
        self.next = (self.next + 1) % N;
        self.play_on(slot, shot, now);
        slot
    }

    /// Silence every voice now, whatever it was doing.
    pub fn silence_all(&mut self) {
        for (slot, voice) in self.voices.iter().enumerate() {
            voice.set_volume(Volume::SILENCE, Volume::SILENCE);
            self.off_at[slot] = None;
        }
    }

    /// The voice in `slot`.
    pub const fn voice(&self, slot: usize) -> Voice {
        self.voices[slot]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rate_hz: u32, frames: u32) -> Sample {
        Sample {
            addr: SpuAddr::new(0x1010),
            rate_hz,
            frames,
        }
    }

    #[test]
    fn the_parking_block_ends_repeats_and_marks_a_loop_start() {
        // LOOP-START is the bit that separates this from psx-spu's shared
        // silence block, which the browse voice ran straight past on silicon
        // despite the repeat register naming it. The hardware latches the
        // repeat address off a block carrying 0x04 as it decodes it, which
        // owes nothing to a register written before key-on.
        assert_eq!(PARKING_TAIL[1] & 0x01, 0x01, "END");
        assert_eq!(PARKING_TAIL[1] & 0x02, 0x02, "REPEAT");
        assert_eq!(PARKING_TAIL[1] & 0x04, 0x04, "LOOP-START");
        assert_eq!(PARKING_TAIL[0], 0x00, "shift and filter zero");
        assert!(
            PARKING_TAIL[2..].iter().all(|&b| b == 0),
            "decodes to silence"
        );
    }

    #[test]
    fn a_samples_length_follows_its_block_count_and_rate() {
        // 126 blocks at 44100 Hz is ui_beep: 3528 samples, 0.08 s, so five
        // ticks of a 60 Hz clock once the margin is on.
        let s = sample(44100, 126);
        assert_eq!(s.len_samples(), 3528);
        assert_eq!(s.ticks_at(60), Some(3528 * 60 / 44100 + TAIL_TICKS));
    }

    #[test]
    fn a_zero_rate_sample_does_not_divide_by_zero() {
        assert_eq!(sample(0, 10).ticks_at(60), Some(10 * 28 * 60 + TAIL_TICKS));
    }

    #[test]
    fn stretching_the_pitch_stretches_the_cutoff() {
        let s = sample(44100, 394); // jump, 0.25 s
        let plain = OneShot::new(s, Volume::MAX);
        let half = plain.with_pitch(Pitch::raw(0x0800));
        // Half the pitch is twice the duration, so the cutoff has to wait
        // twice as long or it clips the sound it is meant to be following.
        assert_eq!(half.ticks(60).unwrap(), plain.ticks(60).unwrap() * 2);
    }

    #[test]
    fn a_pitch_above_the_hardware_ceiling_is_timed_at_the_ceiling() {
        // SB2 played 0x3FFF and 0x5000 back at the same 15169 Hz on silicon.
        // Timing the cutoff off 0x5000 would cut the sound at four fifths.
        let s = sample(44100, 394);
        let asked = OneShot::new(s, Volume::MAX).with_pitch(Pitch::raw(0x5000));
        let capped = OneShot::new(s, Volume::MAX).with_pitch(Pitch::MAX);
        assert_eq!(asked.ticks(60), capped.ticks(60));
    }

    #[test]
    fn a_zero_pitch_falls_back_rather_than_dividing_by_zero() {
        let s = sample(44100, 394);
        let shot = OneShot::new(s, Volume::MAX).with_pitch(Pitch::raw(0));
        assert_eq!(shot.ticks(60), s.ticks_at(60));
    }

    #[test]
    fn a_sample_of_unknown_length_gets_no_cutoff() {
        // VoXide's cooked bank records an offset and a rate but no block
        // count, so there is nothing to compute a deadline from. Better to
        // say so than to invent a length and clip the sound.
        let s = Sample::resident(SpuAddr::new(0x1010), 22050, 0);
        assert_eq!(s.ticks_at(60), None);
        assert_eq!(OneShot::new(s, Volume::MAX).ticks(60), None);
    }

    #[test]
    fn the_cutoff_survives_the_tick_counter_wrapping() {
        assert!(!expired(10, 20));
        assert!(expired(20, 20));
        assert!(expired(21, 20));
        // Deadline set just before the counter rolls over, checked just
        // after: the naive `now >= deadline` says "not yet" here and leaves
        // the voice running for the rest of time.
        assert!(!expired(u32::MAX - 5, u32::MAX - 2));
        assert!(expired(2, u32::MAX - 2));
    }
}
