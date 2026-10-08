//! SB2: the SPU diagnostic -- does RAM hold our data, and does it sound right.
//!
//! Every broken sound on the demo disc shares one thing and avoids one
//! thing. Celeste's looped wavetables, VoXide's sample bank and the
//! launcher's blips all play data the CPU uploaded into SPU RAM, and all
//! are wrong on console while fine on the emulator. CD-DA -- the one audio
//! path that never stores anything in SPU RAM -- is the one that works.
//! SB1 already left a fingerprint too: its readback hash of the uploaded
//! blip came back 11FD257F on the emulator and 80F1AE57 on the console,
//! from identical source bytes.
//!
//! So this probe runs two passes, in the order that narrows fastest.
//!
//! PASS 1, silent and instant: upload a known pattern, read it back,
//! compare. Every word of the pattern says where it lives, so a shifted
//! readback reports its own offset instead of merely "wrong". The trap is
//! blaming the upload for what may be the readback, so the stages vary one
//! thing at a time -- DMA in/DMA out (the SDK's path), PIO in/DMA out, DMA
//! in/PIO out, PIO both ways, a far address, one lone ADPCM block, and the
//! same region read twice to see whether the reader is even stable.
//!
//! PASS 2, audible and 30 seconds long: tones whose correct frequency is
//! arithmetic. The wavetable is synthesised here, two ADPCM blocks, 56
//! samples, two square cycles, filter 0 so the nibbles ARE the waveform.
//! One cycle per 28 samples means unity pitch sounds at 44100/28 = 1575 Hz
//! exactly, and any pitch at `1575 * pitch/0x1000`. Each segment is 1.5 s
//! of tone then 0.5 s of silence, so a recording cuts apart on the gaps
//! alone and `tools/analyse-tone-ladder.py` can measure every one.
//!
//! That is the point of running both: the QR carries what the REGISTERS
//! said, an OBS capture carries what the SPEAKER got. A segment whose
//! registers look right and whose sound is wrong is silicon behaviour; one
//! whose registers are already wrong is our own bug.
//!
//! One caution about pass 1, learned the moment it first ran. On the
//! EMULATOR every stage reports every word wrong, reading back 0000FFFF
//! where the pattern says 5A0000xx. Uninitialised SPU RAM reads as FFFF,
//! and a reader that packs one 16-bit word into each 32-bit slot would
//! produce exactly that -- so the failure may be the reader (shared with
//! SB1) or the emulator not modelling SPU RAM readback at all, rather
//! than a real upload fault. Pass 1 is therefore a COMPARISON instrument,
//! not a verdict: what matters is whether console and emulator differ,
//! and which of the seven routes differ. Pass 2's audio is the ground
//! truth that settles it, because a tone either comes out at 1575 Hz or
//! it does not.

use psx_rt::interrupts;
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};

use crate::console_tests::record;
use crate::{hex2, spu_dma_read, TimingRecord};

// ---- Pass 1: SPU RAM integrity -----------------------------------------

const RAM_STAGES: usize = 7;
const RAM_FIELDS: usize = 4;
/// 4 KiB: more than a wavetable, and enough that a periodic corruption
/// shows its period.
const PATTERN_WORDS: usize = 1024;
/// The short stage: one ADPCM block, the size of a sample's tail.
const SHORT_WORDS: usize = 4;
const ADDR_LOW: u32 = 0x1010;
const ADDR_HIGH: u32 = 0x3_0000;

static mut SOURCE: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];
static mut READBACK: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];
static mut READBACK2: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];

// ---- Pass 2: the tone ladder -------------------------------------------

const TONE_SEGMENTS: usize = 22;
const TONE_FIELDS: usize = 4;
const TONE_FRAMES: u16 = 60;
const GAP_FRAMES: u16 = 4;
/// HOLD runs long enough for slow loop degradation to show.
const HOLD_TONE_FRAMES: u16 = 90;
const EARLY_FRAME: u16 = 8;

/// 64 blocks, not 2. The console keeps playing something other than this
/// table: its repeat address reads back as 0x1A60 in every segment and in
/// two separate runs -- a fixed leftover, not a wandering voice -- and
/// REPEXPL proved the register accepts 0x1010 by hand and the tone still
/// died. Meanwhile the RAM pass's 4 KiB uploads read back as our own
/// pattern while a 32-byte one read back as zero. So the remaining
/// suspect is transfer SIZE: a two-block upload may simply not land. The
/// waveform is unchanged -- still one square cycle per 28 samples, so
/// unity is still 1575 Hz -- there is just a kilobyte of it, which is the
/// same order as a real wavetable.
const TABLE_BLOCKS: usize = 64;
const TABLE_BYTES: usize = TABLE_BLOCKS * 16;
const UNITY_PITCH: u16 = 0x1000;
/// Shift 1 rather than 0: a capture chain with a little gain then cannot
/// clip the tone into a square of its own.
const TONE_SHIFT: u8 = 1;
const HIGH_NIBBLE: u8 = 0x7;
const LOW_NIBBLE: u8 = 0x8; // -8 in 4-bit two's complement
const SPU_TABLE_ADDR: u32 = 0x1010;
const SPU_TABLE2_ADDR: u32 = SPU_TABLE_ADDR + TABLE_BYTES as u32;
/// A two-block copy of the same waveform, uploaded by the same call, at
/// its own address. Two things changed at once this round -- the upload
/// path learned to wait for the SPU, and the table grew from 32 bytes to
/// a kilobyte -- so a correct ladder would not say which fixed it. This
/// segment keeps the small size and changes nothing else: right means
/// size never mattered and the missing waits were the whole bug, wrong
/// means small uploads are still losing data on their own.
const SPU_SMALL_ADDR: u32 = SPU_TABLE2_ADDR + TABLE_BYTES as u32;
const SMALL_BLOCKS: usize = 2;

/// Termination pair, for segments 15..19.
///
/// Two identical four-block tones laid down twice. PARKED is followed by a
/// self-looping silent block carrying LOOP-START; UNPARKED is followed by a
/// loud, obviously different tone and no parking block at all. Everything else
/// about them is the same, so the only thing the two segments can disagree
/// about is what the hardware does when a voice reaches the end of its data.
///
/// This is the measurement SB2 could not make. It read the repeat register and
/// found it correct on 2026-08-03 while voices were audibly running into the
/// next sample, because the register says where the hardware WOULD jump, not
/// whether it did.
const TERM_BLOCKS: usize = 4;
const TERM_BYTES: usize = TERM_BLOCKS * 16;
/// PARKED's tone, then its parking block.
const SPU_TERM_PARKED_ADDR: u32 = SPU_SMALL_ADDR + (SMALL_BLOCKS * 16) as u32;
const SPU_TERM_PARK_BLOCK: u32 = SPU_TERM_PARKED_ADDR + TERM_BYTES as u32;
/// UNPARKED's tone, then the neighbour it must not reach.
const SPU_TERM_UNPARKED_ADDR: u32 = SPU_TERM_PARK_BLOCK + 16;
const SPU_TERM_NEIGHBOUR_ADDR: u32 = SPU_TERM_UNPARKED_ADDR + TERM_BYTES as u32;
const VOICE: u8 = 0;
const MAX_VOICES: u8 = 8;
const TONE_VOLUME: Volume = Volume::linear(1, 4);
const PITCH_LADDER: [u16; 7] = [0x0400, 0x0800, 0x1000, 0x2000, 0x3000, 0x3FFF, 0x5000];

// ---- Payload ------------------------------------------------------------

const WORDS: usize = RAM_STAGES * RAM_FIELDS + TONE_SEGMENTS * TONE_FIELDS;

/// rec sb2_tone: late_envelope, repeat_address, start_address (22 records, 0x440-0x455)
pub(crate) const SB2_TONE: u16 = 0x440;
/// rec sb2_early: early_envelope_or_endx, late_pitch, table_word (22 records, 0x460-0x475)
pub(crate) const SB2_EARLY: u16 = 0x460;
/// rec sb2_ram: mismatching_words, first_bad_index, word_read_there (7 records, 0x480-0x486)
pub(crate) const SB2_RAM: u16 = 0x480;
/// Records the probe leaves.
pub(crate) const RECORD_COUNT: usize = TONE_SEGMENTS * 2 + RAM_STAGES;
pub(crate) type Records = [TimingRecord; RECORD_COUNT];

pub(crate) struct SpuProbe {
    /// 0..RAM_STAGES = pass 1, then pass 2 segments, then done.
    step: u8,
    frame: u16,
    complete: bool,
    run: u8,
    words: [u32; WORDS],
    /// Words 0 and 4 of the uploaded table, read back from SPU RAM.
    table_back: [u32; 2],
}

impl SpuProbe {
    pub(crate) const fn new() -> Self {
        Self {
            step: 0,
            frame: 0,
            complete: false,
            run: 0,
            words: [0; WORDS],
            table_back: [0; 2],
        }
    }

    pub(crate) fn start(&mut self) {
        self.step = 0;
        self.frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.words = [0; WORDS];
        self.table_back = [0; 2];
        spu::init();
        spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
        spu::enable_cd_audio(false);
        // Every word says where it lives, so a shifted readback reports
        // its own offset rather than just "wrong".
        let source = unsafe { &mut *core::ptr::addr_of_mut!(SOURCE) };
        for (index, word) in source.iter_mut().enumerate() {
            *word = 0x5A00_0000 | (index as u32 & 0x00FF_FFFF);
        }
        tty::println("hardware-tests: sb2 begin spu diagnostic");
        self.begin_step();
    }

    /// Run the whole probe, a frame at a time, and return its records.
    pub(crate) fn run(&mut self) -> Records {
        self.start();
        let mut tick = 0u32;
        while !self.complete {
            self.update(tick);
            interrupts::wait_vblank();
            tick += 1;
        }
        self.records()
    }

    /// The results as records: two per tone segment (`0x440 + segment`:
    /// late envelope, repeat address, start address; `0x460 + segment`: the
    /// early envelope or, for the termination segments, ENDX low half, the
    /// pitch register, the table readback word) and one per RAM stage
    /// (`0x480 + stage`: mismatching words, first bad index, what it read).
    fn records(&self) -> Records {
        let mut out = [record(SB2_TONE, 0, 0, 0); RECORD_COUNT];
        for segment in 0..TONE_SEGMENTS {
            let at = segment * TONE_FIELDS;
            let early = self.words[at + 1];
            let late = self.words[at + 2];
            let loops = self.words[at + 3];
            let termination = (15..19).contains(&segment);
            out[segment * 2] = record(
                SB2_TONE + segment as u16,
                late & 0xFFFF,
                loops >> 16,
                loops & 0xFFFF,
            );
            out[segment * 2 + 1] = record(
                SB2_EARLY + segment as u16,
                if termination {
                    early & 0xFFFF
                } else {
                    early & 0xFFFF
                },
                late >> 16,
                self.table_back[segment & 1] & 0xFFFF,
            );
        }
        for stage in 0..RAM_STAGES {
            let at = TONE_SEGMENTS * TONE_FIELDS + stage * RAM_FIELDS;
            out[TONE_SEGMENTS * 2 + stage] = record(
                SB2_RAM + stage as u16,
                self.words[at + 3],
                self.words[at + 1],
                self.words[at + 2] & 0xFFFF,
            );
        }
        out
    }

    pub(crate) fn update(&mut self, tick: u32) {
        if self.complete {
            return;
        }
        let step = self.step as usize;
        if step >= TONE_SEGMENTS {
            // The RAM pass runs LAST now. It drives the transfer registers
            // by hand, and the first console capture showed the tone ladder
            // silent behind it even with a reset in between -- so the tones
            // are measured on a virgin SPU first, and whatever pass 1 does
            // to the machine can only affect pass 1's own numbers.
            self.run_ram_stage(step - TONE_SEGMENTS);
            self.advance();
            return;
        }

        let segment = step;
        let tone_frames = if segment == 8 {
            HOLD_TONE_FRAMES
        } else {
            TONE_FRAMES
        };
        match segment {
            // SYNC: three 10-frame bursts, for finding t=0 in the audio.
            0 => match self.frame {
                0 | 20 | 40 => key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR),
                10 | 30 | 50 => Voice::release(Voice::new(VOICE).mask()),
                _ => {}
            },
            // REKEY at PICO-8's note rate.
            9 if self.frame < tone_frames && self.frame.is_multiple_of(4) => {
                key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR);
            }
            // ADDRSWAP: new start address, no key_on. Silicon should honour
            // it only at the loop point, and the recording times that.
            10 if self.frame == tone_frames / 2 => {
                Voice::new(VOICE).set_start_addr(SpuAddr::new(SPU_TABLE2_ADDR));
            }
            // VOICES: 1, then 4, then 8 keyed together.
            11 => {
                if self.frame == tone_frames / 3 {
                    for v in 1..4 {
                        key_voice(v, UNITY_PITCH, SPU_TABLE_ADDR);
                    }
                } else if self.frame == 2 * tone_frames / 3 {
                    for v in 4..MAX_VOICES {
                        key_voice(v, UNITY_PITCH, SPU_TABLE_ADDR);
                    }
                }
            }
            // PARKED: a one-shot followed by a self-looping silent block.
            // ENDX is cleared at key-on so the payload's "early" word reports
            // only what this voice did during this segment.
            15 if self.frame == 0 => {
                Voice::clear_ended(0x00FF_FFFF);
                key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
            }
            // UNPARKED: the same one-shot with a loud neighbour behind it and
            // nothing to park on. Audible on a capture as well as readable in
            // the payload, because a voice that runs on drops an octave.
            16 if self.frame == 0 => {
                Voice::clear_ended(0x00FF_FFFF);
                key_voice(VOICE, UNITY_PITCH, SPU_TERM_UNPARKED_ADDR);
            }
            // ENDXBIT: does the sticky END flag set for the right voice, and
            // only that voice? Keys voice 1 rather than 0, so a payload that
            // reports bit 0 is reporting a stale flag.
            17 if self.frame == 0 => {
                Voice::clear_ended(0x00FF_FFFF);
                key_voice(1, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
            }
            // ENVZERO: leave the voice alone after it ends and read the
            // envelope late. A one-shot that terminated should be at zero; one
            // still reading forward will not be.
            18 if self.frame == 0 => {
                Voice::clear_ended(0x00FF_FFFF);
                key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
            }
            // KEYVOL: does key_on restore a voice whose volume was written to
            // zero while it played?
            //
            // The demo disc's v0.11 pressing shipped a browse blip that sounded
            // once and never again. Its cutoff silenced the voice by writing
            // volume 0, and the next key_on did not put it back, so every blip
            // after the first was mute. psx-sfx sets volume on every key-on
            // because of it, and that rule is currently a comment rather than a
            // measurement.
            //
            // Key, silence mid-tone, then key again touching nothing else. A
            // late envelope of zero means volume does not survive a key-on and
            // the rule is real; a live envelope means it does.
            19 => match self.frame {
                0 => key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR),
                20 => Voice::new(VOICE).set_volume(Volume::SILENCE, Volume::SILENCE),
                40 => Voice::start(Voice::new(VOICE).mask()),
                _ => {}
            },
            // RETRIG: key a voice that is still sounding, which is what the
            // carousel does on every turn and what a player leaning on the pad
            // does several times a second.
            //
            // Re-keys every 12 frames without touching anything else, so the
            // envelope read late says whether silicon restarts cleanly or
            // accumulates state across an interrupted sample.
            20 => {
                if self.frame.is_multiple_of(12) && self.frame < tone_frames {
                    Voice::start(Voice::new(VOICE).mask());
                }
                if self.frame == 0 {
                    key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
                }
            }
            // XFERLIVE: upload to SPU RAM while a voice is playing from a
            // different address.
            //
            // hl-psx streams a map's dialogue in while sounds are playing and
            // VoXide streams its whole bank at boot, so a transfer that
            // disturbs playback would show up as a stutter nobody could place.
            // The upload targets the far end of RAM, well clear of the tone.
            21 => {
                if self.frame == 0 {
                    key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
                }
                if self.frame == 30 {
                    let small = unsafe { &*core::ptr::addr_of!(SOURCE) };
                    let bytes =
                        unsafe { core::slice::from_raw_parts(small.as_ptr() as *const u8, 512) };
                    spu::upload_adpcm(SpuAddr::new(ADDR_HIGH), bytes);
                }
            }
            _ => {}
        }

        let at = segment * TONE_FIELDS;
        if self.frame == EARLY_FRAME {
            let (pitch, env) = read_voice(VOICE);
            self.words[at] = ((segment as u32) << 24) | (tick & 0x00FF_FFFF);
            self.words[at + 1] = ((pitch as u32) << 16) | env as u32;
        }
        // The termination segments report ENDX instead of an early sample: the
        // question they ask is whether the voice reached its own terminator,
        // and the sticky flag is the only direct evidence of that.
        if (15..19).contains(&segment) && self.frame + 2 == tone_frames {
            self.words[at + 1] = Voice::ended_voices();
        }
        if self.frame + 2 == tone_frames {
            // ENDXBIT keys voice 1, so read the voice the segment actually
            // drove or the row describes a voice that was never touched.
            let observed = if segment == 17 { 1 } else { VOICE };
            let (pitch, env) = read_voice(observed);
            self.words[at + 2] = ((pitch as u32) << 16) | env as u32;
            let base = psx_hw::spu::BASE + observed as u32 * 16;
            // +6 is the START address. The first cut read +4 here, which is
            // the PITCH register, so the payload's "start" column was the
            // pitch repeated -- harmless but misleading in a capture.
            let start = unsafe { psx_io::read_u16(base + 6) };
            // The loop truth: where silicon jumps at an END block, in
            // 8-byte units. It should equal the table's own start.
            let repeat = unsafe { psx_io::read_u16(base + 14) };
            self.words[at + 3] = ((repeat as u32) << 16) | start as u32;
        }
        if self.frame == tone_frames {
            Voice::release(all_voices_mask());
        }

        self.frame = self.frame.saturating_add(1);
        if self.frame >= tone_frames + GAP_FRAMES {
            self.advance();
        }
    }

    fn advance(&mut self) {
        self.frame = 0;
        if (self.step as usize) + 1 < TONE_SEGMENTS + RAM_STAGES {
            self.step += 1;
            self.begin_step();
        } else {
            Voice::release(all_voices_mask());
            Voice::set_noise_mask(0);
            self.complete = true;
        }
    }

    fn begin_step(&mut self) {
        let step = self.step as usize;
        if step >= TONE_SEGMENTS {
            return;
        }
        let segment = step;
        if segment == 0 {
            // Entering pass 2 with a CLEAN SPU. Pass 1 deliberately drives
            // the transfer registers by hand, and the first console run
            // proved that state outlives the pass: the whole ladder was
            // silent behind it. Reset before the tables go up, so pass 2
            // measures playback rather than pass 1's leftovers.
            spu::init();
            spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
            spu::enable_cd_audio(false);
            upload_tables();
            // Read the table straight back. The console's repeat address
            // came back pointing at table TWO, which means the voice ran
            // past table one's END flag -- so the first question is whether
            // the flags are even in SPU RAM. Word 0 holds the header and
            // flags bytes of block 0; word 4 holds block 1's.
            // PIO, not DMA. The console's RAM pass showed the DMA reader
            // returning 0000FFFF for a region the PIO reader read as our
            // own pattern, so a DMA readback of the table proves nothing.
            let mut back = [0u32; 8];
            pio_read(SPU_TABLE_ADDR, &mut back);
            self.table_back = [back[0], back[4]];
        }
        Voice::release(all_voices_mask());
        Voice::set_noise_mask(0);
        match segment {
            0 => {} // SYNC keys itself
            s @ 1..=7 => key_voice(VOICE, PITCH_LADDER[s - 1], SPU_TABLE_ADDR),
            8..=11 => key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR),
            12 => {
                Voice::set_noise_mask(1 << VOICE);
                spu::set_noise_clock(8, 2);
                key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR);
            }
            // The console read this voice's REPEAT address back as 034C
            // (0x1A60) -- two and a half kilobytes past a 32-byte table,
            // so the voice ran clean past its own END flag and latched a
            // loop-start it found in whatever the launcher left in SPU
            // RAM. Every measured tone was proportional to its pitch but
            // 2.41x the arithmetic, which is what playing the wrong bytes
            // at the right rate sounds like. This segment writes the
            // repeat register by hand straight after key-on: if the tone
            // comes back at 1575 Hz then the flags are in RAM and it is
            // the LATCH that silicon is not doing -- the same mechanism
            // Celeste's wavetables depend on.
            14 => key_voice(VOICE, UNITY_PITCH, SPU_SMALL_ADDR),
            13 => {
                key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR);
                let base = psx_hw::spu::BASE + VOICE as u32 * 16;
                unsafe { psx_io::write_u16(base + 14, (SPU_TABLE_ADDR / 8) as u16) };
            }
            _ => {}
        }
        tty::print("hardware-tests: sb2 tone seg=");
        tty::print(hex2(segment as u8).as_str());
        tty::print(" ");
        tty::println("");
    }

    /// One upload/readback combination, compared word for word.
    fn run_ram_stage(&mut self, stage: usize) {
        let source = unsafe { &*core::ptr::addr_of!(SOURCE) };
        let readback = unsafe { &mut *core::ptr::addr_of_mut!(READBACK) };
        let readback2 = unsafe { &mut *core::ptr::addr_of_mut!(READBACK2) };
        let (addr, words) = match stage {
            4 => (ADDR_HIGH, PATTERN_WORDS),
            5 => (ADDR_LOW, SHORT_WORDS),
            _ => (ADDR_LOW, PATTERN_WORDS),
        };
        let src = &source[..words];
        // Scrub first: a read that transfers NOTHING then shows as zeros
        // rather than as the last stage's data.
        readback[..words].fill(0);
        readback2[..words].fill(0);

        let bytes = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u8, words * 4) };
        match stage {
            1 | 2 => pio_write(addr, src),
            _ => spu::upload_adpcm(SpuAddr::new(addr), bytes),
        }
        match stage {
            2 | 3 => pio_read(addr, &mut readback[..words]),
            _ => spu_dma_read(addr, &mut readback[..words]),
        }
        if stage == 6 {
            // The same region again by the same route: two reads that
            // disagree with each other indict the reader outright.
            spu_dma_read(addr, &mut readback2[..words]);
        }

        let mut first_bad = u32::MAX;
        let mut got_at = 0u32;
        let mut bad = 0u32;
        for index in 0..words {
            if readback[index] != src[index] {
                if first_bad == u32::MAX {
                    first_bad = index as u32;
                    got_at = readback[index];
                }
                bad += 1;
            }
        }
        if stage == 6 {
            // Stage 6's verdict is read-vs-read, not read-vs-source.
            bad = 0;
            for index in 0..words {
                if readback[index] != readback2[index] {
                    bad += 1;
                }
            }
        }

        let at = TONE_SEGMENTS * TONE_FIELDS + stage * RAM_FIELDS;
        self.words[at] = ((stage as u32) << 24) | words as u32;
        self.words[at + 1] = first_bad;
        self.words[at + 2] = got_at;
        self.words[at + 3] = bad;

        tty::print("hardware-tests: sb2 ram=");
        tty::print(hex2(stage as u8).as_str());
        tty::print(" bad=");
        tty::println(if bad == 0 { "0" } else { "some" });
    }
}

fn all_voices_mask() -> u32 {
    (1u32 << MAX_VOICES) - 1
}

fn read_voice(index: u8) -> (u16, u16) {
    let base = psx_hw::spu::BASE + index as u32 * 16;
    let pitch = unsafe { psx_io::read_u16(base + 4) };
    let env = unsafe { psx_io::read_u16(base + 12) };
    (pitch, env)
}

/// Key one voice on the synthetic table, with Celeste's own envelope:
/// instant attack, full sustain, fastest release -- so what a recording
/// hears is the wavetable, not an envelope shape.
fn key_voice(index: u8, pitch: u16, addr: u32) {
    let voice = Voice::new(index);
    voice.set_volume(TONE_VOLUME, TONE_VOLUME);
    voice.set_pitch(Pitch::raw(pitch));
    voice.set_start_addr(SpuAddr::new(addr));
    voice.set_adsr(Adsr {
        lower: 0x000F,
        upper: 0x0000,
    });
    Voice::start(voice.mask());
}

/// Build both square tables and upload them. Block 0 carries loop-start,
/// the last block loop-end+repeat: the same shape Celeste's wavetables use.
fn upload_tables() {
    let mut table = [0u8; TABLE_BYTES];
    build_square(&mut table, 1);
    spu::upload_adpcm(SpuAddr::new(SPU_TABLE_ADDR), &table);
    // An octave down, for ADDRSWAP: a swap that takes effect is audible
    // as a drop, not as a subtlety.
    let mut table2 = [0u8; TABLE_BYTES];
    build_square(&mut table2, 2);
    spu::upload_adpcm(SpuAddr::new(SPU_TABLE2_ADDR), &table2);
    // The termination pair. Both tones are identical four-block one-shots
    // whose last block raises END and nothing else -- exactly the shape every
    // cooked .psau on the disc has, and the shape that was wandering.
    //
    // PARKED is followed by a self-looping silent block carrying LOOP-START,
    // which is what psx-sfx and hl-psx append now. UNPARKED is followed by a
    // deliberately loud neighbour and no parking block, which is what every
    // packed bank looked like before. If the two segments read back the same
    // repeat address, the parking block is doing nothing and the fix is
    // theatre. If PARKED reports the park block's own address and UNPARKED
    // does not, the latch is real and it is what stops the wandering.
    let mut term = [0u8; TERM_BYTES];
    build_square(&mut term, 1);
    // Plain END on the last block, no repeat and no loop-start: a one-shot,
    // not a loop. build_square writes a looping shape, so undo it here.
    term[1] = 0x00;
    term[(TERM_BLOCKS - 1) * 16 + 1] = 0x01;
    spu::upload_adpcm(SpuAddr::new(SPU_TERM_PARKED_ADDR), &term);
    let park = [0x00u8, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    spu::upload_adpcm(SpuAddr::new(SPU_TERM_PARK_BLOCK), &park);
    spu::upload_adpcm(SpuAddr::new(SPU_TERM_UNPARKED_ADDR), &term);
    // The neighbour: an octave down and unmissable, so a capture can hear a
    // voice that ran past its own data as well as read it in the payload.
    let mut neighbour = [0u8; TERM_BYTES];
    build_square(&mut neighbour, 2);
    spu::upload_adpcm(SpuAddr::new(SPU_TERM_NEIGHBOUR_ADDR), &neighbour);

    // The size control: same waveform, same call, 32 bytes.
    let mut small = [0u8; SMALL_BLOCKS * 16];
    for block in 0..SMALL_BLOCKS {
        let at = block * 16;
        small[at] = TONE_SHIFT;
        small[at + 1] = if block == 0 { 0x04 } else { 0x03 };
        for sample in 0..28 {
            let index = block * 28 + sample;
            let nibble = if (index % 28) * 2 < 28 {
                HIGH_NIBBLE
            } else {
                LOW_NIBBLE
            };
            let byte = at + 2 + sample / 2;
            if sample % 2 == 0 {
                small[byte] |= nibble
            } else {
                small[byte] |= nibble << 4
            }
        }
    }
    spu::upload_adpcm(SpuAddr::new(SPU_SMALL_ADDR), &small);
}

/// `half_period_blocks == 1` gives one square cycle per 28 samples (1575 Hz
/// at unity pitch); 2 gives one cycle per 56 samples, an octave down.
fn build_square(table: &mut [u8], half_period_blocks: usize) {
    let period = 28 * half_period_blocks;
    // The table's own length decides the block count: this builds the 64-block
    // tables and the 4-block termination pair alike. (It used to loop over
    // TABLE_BLOCKS regardless, and nothing ever reached it, because the first
    // segment was never begun: the tone segments played whatever the SPU RAM
    // held. See hardware-test-versions.md, v2.0.)
    let blocks = table.len() / 16;
    for block in 0..blocks {
        let at = block * 16;
        table[at] = TONE_SHIFT; // filter 0
                                // Loop-start on the first block, END+REPEAT on the LAST, nothing
                                // in between. With only two blocks "not first" and "last" were
                                // the same thing; at 64 they are not, and marking every middle
                                // block as END would have ended the sample after one of them.
        table[at + 1] = if block == 0 {
            0x04
        } else if block == blocks - 1 {
            0x03
        } else {
            0x00
        };
        for sample in 0..28 {
            let index = block * 28 + sample;
            let nibble = if (index % period) * 2 < period {
                HIGH_NIBBLE
            } else {
                LOW_NIBBLE
            };
            let byte = at + 2 + sample / 2;
            if sample % 2 == 0 {
                table[byte] |= nibble;
            } else {
                table[byte] |= nibble << 4;
            }
        }
    }
}

/// Upload by CPU stores to the SPU's transfer port: no DMA channel, no
/// block sizing, nothing but the SPU's own FIFO.
fn pio_write(addr: u32, words: &[u32]) {
    unsafe {
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0000);
        psx_io::write_u16(psx_hw::spu::TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0004);
        let cnt = psx_io::read_u16(psx_hw::spu::SPUCNT);
        psx_io::write_u16(psx_hw::spu::SPUCNT, (cnt & !0x0030) | 0x0010); // manual write
        for &word in words {
            psx_io::write_u16(psx_hw::spu::TRANSFER_DATA, word as u16);
            psx_io::write_u16(psx_hw::spu::TRANSFER_DATA, (word >> 16) as u16);
        }
        psx_io::write_u16(psx_hw::spu::SPUCNT, cnt & !0x0030);
        // 0x0004, never 0: see the note in pio_read.
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0004);
    }
}

/// Read back through the same port. If this and the DMA path disagree,
/// that difference is the SPU's read pipeline.
fn pio_read(addr: u32, out: &mut [u32]) {
    unsafe {
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0000);
        psx_io::write_u16(psx_hw::spu::TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0004);
        let cnt = psx_io::read_u16(psx_hw::spu::SPUCNT);
        psx_io::write_u16(psx_hw::spu::SPUCNT, (cnt & !0x0030) | 0x0030); // manual read
        for word in out.iter_mut() {
            let lo = psx_io::read_u16(psx_hw::spu::TRANSFER_DATA) as u32;
            let hi = psx_io::read_u16(psx_hw::spu::TRANSFER_DATA) as u32;
            *word = lo | (hi << 16);
        }
        psx_io::write_u16(psx_hw::spu::SPUCNT, cnt & !0x0030);
        psx_io::write_u16(psx_hw::spu::TRANSFER_CTRL, 0x0000);
    }
}
