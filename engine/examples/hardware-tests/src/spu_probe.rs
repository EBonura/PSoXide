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

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::photo::crc32;
use crate::{hex2, hex8, spu_dma_read};

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

/// SPU transfer registers, for the PIO paths.
const SPU_TRANSFER_ADDR: u32 = 0x1F80_1DA6;
const SPU_TRANSFER_DATA: u32 = 0x1F80_1DA8;
const SPU_CNT: u32 = 0x1F80_1DAA;
const SPU_TRANSFER_CTRL: u32 = 0x1F80_1DAC;

static mut SOURCE: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];
static mut READBACK: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];
static mut READBACK2: [u32; PATTERN_WORDS] = [0; PATTERN_WORDS];

// ---- Pass 2: the tone ladder -------------------------------------------

const TONE_SEGMENTS: usize = 19;
const TONE_FIELDS: usize = 4;
const TONE_FRAMES: u16 = 90;
const GAP_FRAMES: u16 = 30;
/// HOLD runs long enough for slow loop degradation to show.
const HOLD_TONE_FRAMES: u16 = 300;
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
/// 44100 / 28 samples per cycle, at SPU pitch 0x1000.
const UNITY_HZ: u32 = 1575;
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
const QR_VERSION_NUM: u8 = 19;
const QR_VERSION: Version = Version::new(QR_VERSION_NUM);
const QR_SIZE: usize = 4 * QR_VERSION_NUM as usize + 17;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 20 + WORDS * 4 + 4;
const BASE64_LEN: usize = BINARY_LEN.div_ceil(3) * 4;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

/// Byte-mode capacity at ECC Medium for `QR_VERSION_NUM`, from the ISO table.
/// Version 17 holds 504 and v0.12 shipped a 519-byte payload, so `encode_text`
/// failed and the screen drew QR ENCODE FAILED. Adding a segment must break the
/// build, not the burn.
const QR_BYTE_CAPACITY: usize = 624;
const _: () = assert!(QR_TEXT_MAX <= QR_BYTE_CAPACITY, "SB2 QR payload too big");
const _: () = assert!((QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE <= 240, "SB2 QR too tall");

pub(crate) struct SpuProbe {
    /// 0..RAM_STAGES = pass 1, then pass 2 segments, then done.
    step: u8,
    frame: u16,
    complete: bool,
    run: u8,
    words: [u32; WORDS],
    /// Words 0 and 4 of the uploaded table, read back from SPU RAM.
    table_back: [u32; 2],
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
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
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn start(&mut self) {
        self.step = 0;
        self.frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.words = [0; WORDS];
        self.table_back = [0; 2];
        self.qr_size = 0;
        spu::init();
        spu::set_main_volume(Volume::MAX, Volume::MAX);
        spu::enable_cd_audio(false);
        // Every word says where it lives, so a shifted readback reports
        // its own offset rather than just "wrong".
        let source = unsafe { &mut *core::ptr::addr_of_mut!(SOURCE) };
        for (index, word) in source.iter_mut().enumerate() {
            *word = 0x5A00_0000 | (index as u32 & 0x00FF_FFFF);
        }
        tty::println("hardware-tests: sb2 begin spu diagnostic");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
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
        let tone_frames = if segment == 8 { HOLD_TONE_FRAMES } else { TONE_FRAMES };
        match segment {
            // SYNC: three 10-frame bursts, for finding t=0 in the audio.
            0 => match self.frame {
                0 | 20 | 40 => key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR),
                10 | 30 | 50 => Voice::key_off(Voice::new(VOICE).mask()),
                _ => {}
            },
            // REKEY at PICO-8's note rate.
            9 => {
                if self.frame < tone_frames && self.frame % 4 == 0 {
                    key_voice(VOICE, UNITY_PITCH, SPU_TABLE_ADDR);
                }
            }
            // ADDRSWAP: new start address, no key_on. Silicon should honour
            // it only at the loop point, and the recording times that.
            10 => {
                if self.frame == tone_frames / 2 {
                    Voice::new(VOICE).set_start_addr(SpuAddr::new(SPU_TABLE2_ADDR));
                }
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
            15 => {
                if self.frame == 0 {
                    Voice::clear_ended(0x00FF_FFFF);
                    key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
                }
            }
            // UNPARKED: the same one-shot with a loud neighbour behind it and
            // nothing to park on. Audible on a capture as well as readable in
            // the payload, because a voice that runs on drops an octave.
            16 => {
                if self.frame == 0 {
                    Voice::clear_ended(0x00FF_FFFF);
                    key_voice(VOICE, UNITY_PITCH, SPU_TERM_UNPARKED_ADDR);
                }
            }
            // ENDXBIT: does the sticky END flag set for the right voice, and
            // only that voice? Keys voice 1 rather than 0, so a payload that
            // reports bit 0 is reporting a stale flag.
            17 => {
                if self.frame == 0 {
                    Voice::clear_ended(0x00FF_FFFF);
                    key_voice(1, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
                }
            }
            // ENVZERO: leave the voice alone after it ends and read the
            // envelope late. A one-shot that terminated should be at zero; one
            // still reading forward will not be.
            18 => {
                if self.frame == 0 {
                    Voice::clear_ended(0x00FF_FFFF);
                    key_voice(VOICE, UNITY_PITCH, SPU_TERM_PARKED_ADDR);
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
        if segment >= 15 && self.frame + 2 == tone_frames {
            self.words[at + 1] = Voice::voices_ended();
        }
        if self.frame + 2 == tone_frames {
            // ENDXBIT keys voice 1, so read the voice the segment actually
            // drove or the row describes a voice that was never touched.
            let observed = if segment == 17 { 1 } else { VOICE };
            let (pitch, env) = read_voice(observed);
            self.words[at + 2] = ((pitch as u32) << 16) | env as u32;
            let base = psx_io::spu::SPU_BASE + observed as u32 * 16;
            // +6 is the START address. The first cut read +4 here, which is
            // the PITCH register, so the payload's "start" column was the
            // pitch repeated -- harmless but misleading in a capture.
            let start = unsafe { psx_io::read16(base + 6) };
            // The loop truth: where silicon jumps at an END block, in
            // 8-byte units. It should equal the table's own start.
            let repeat = unsafe { psx_io::read16(base + 14) };
            self.words[at + 3] = ((repeat as u32) << 16) | start as u32;
        }
        if self.frame == tone_frames {
            Voice::key_off(all_voices_mask());
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
            Voice::key_off(all_voices_mask());
            Voice::set_noise_mask(0);
            self.complete = true;
            self.encode_qr();
            self.print_payload();
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
            spu::set_main_volume(Volume::MAX, Volume::MAX);
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
        Voice::key_off(all_voices_mask());
        Voice::set_noise_mask(0);
        match segment {
            0 => {}                                                    // SYNC keys itself
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
                let base = psx_io::spu::SPU_BASE + VOICE as u32 * 16;
                unsafe { psx_io::write16(base + 14, (SPU_TABLE_ADDR / 8) as u16) };
            }
            _ => {}
        }
        tty::print("hardware-tests: sb2 tone seg=");
        tty::print(hex2(segment as u8).as_str());
        tty::print(" ");
        tty::println(tone_label(segment as u8));
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
        tty::print(hex8(bad).digits());
        tty::print(" first=");
        tty::println(hex8(first_bad).digits());
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        binary[..4].copy_from_slice(b"SB2B");
        binary[4] = 1;
        binary[5] = RAM_STAGES as u8;
        binary[6] = TONE_SEGMENTS as u8;
        binary[7] = self.run;
        binary[8..12].copy_from_slice(&UNITY_HZ.to_le_bytes());
        binary[12..16].copy_from_slice(&self.table_back[0].to_le_bytes());
        binary[16..20].copy_from_slice(&self.table_back[1].to_le_bytes());
        for (index, word) in self.words.iter().enumerate() {
            let at = 20 + index * 4;
            binary[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        let crc = crc32(&binary[..BINARY_LEN - 4]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        let mut text = [0u8; QR_TEXT_MAX];
        let mut len = 0usize;
        append(&mut text, &mut len, b"SB2/");
        append(&mut text, &mut len, &payload);
        append(&mut text, &mut len, b"/C:");
        append(&mut text, &mut len, hex8(self.binary_crc).digits().as_bytes());
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

    fn print_payload(&self) {
        tty::print("hardware-tests: sb2 crc=");
        tty::println(hex8(self.binary_crc).digits());
    }

    /// The probe's own screen, starting BELOW the suite header.
    ///
    /// The first cut drew a title at y=8 and a subtitle at y=20, straight
    /// over the harness's own "PS1 HARDWARE TESTS" and mode lines, and laid
    /// the RAM verdict out in four columns 76 px wide -- too narrow for a
    /// seven-character label plus an eight-digit value, so those collided
    /// too. Everything here now starts at y=30 and the verdict is one
    /// column, which is legible on a photographed CRT.
    pub(crate) fn draw(&self, font: &FontAtlas) {
        let step = self.step as usize;
        if !self.complete {
            if step < TONE_SEGMENTS {
                let segment = step as u8;
                font.draw_text(8, 30, "PASS 1 OF 2: TONE LADDER", (150, 170, 200));
                font.draw_text(8, 44, "SEG", (140, 160, 190));
                font.draw_text(48, 44, hex2(segment).as_str(), (232, 236, 244));
                font.draw_text(72, 44, tone_label(segment), (96, 200, 255));
                font.draw_text(8, 58, tone_description(segment), (220, 224, 230));
                font.draw_text(8, 72, "EXPECT", (140, 160, 190));
                font.draw_text(72, 72, tone_expectation(segment), (255, 216, 96));
                font.draw_text(8, 92, "1.5S TONE THEN 0.5S SILENCE", (112, 136, 170));
                font.draw_text(8, 106, "RECORD VIDEO *AND AUDIO*", (255, 128, 96));
                font.draw_text(8, 120, "DO NOT TOUCH THE VOLUME MID-RUN", (255, 128, 96));
                return;
            }
            font.draw_text(8, 30, "PASS 2 OF 2: SPU RAM, SILENT", (150, 170, 200));
            let stage = (step - TONE_SEGMENTS) as u8;
            font.draw_text(8, 44, "STAGE", (140, 160, 190));
            font.draw_text(72, 44, ram_label(stage), (96, 200, 255));
            font.draw_text(8, 58, ram_description(stage), (220, 224, 230));
            self.draw_ram_verdict(font, 80);
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

    /// Bad-word counts, one stage per row: label at x=8, count at x=88.
    /// A seven-character label is 56 px and an eight-digit value 64, so a
    /// single column is the only layout that fits both without collision.
    fn draw_ram_verdict(&self, font: &FontAtlas, top: i16) {
        font.draw_text(8, top, "SPU RAM BAD WORDS", (140, 160, 190));
        for stage in 0..RAM_STAGES {
            let bad = self.words[TONE_SEGMENTS * TONE_FIELDS + stage * RAM_FIELDS + 3];
            let colour = if bad == 0 { (96, 240, 128) } else { (255, 96, 96) };
            let y = top + 14 + stage as i16 * 12;
            font.draw_text(8, y, ram_label(stage as u8), (200, 208, 220));
            font.draw_text(88, y, hex8(bad).digits(), colour);
        }
    }

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
    }
}

fn all_voices_mask() -> u32 {
    (1u32 << MAX_VOICES) - 1
}

fn read_voice(index: u8) -> (u16, u16) {
    let base = psx_io::spu::SPU_BASE + index as u32 * 16;
    let pitch = unsafe { psx_io::read16(base + 4) };
    let env = unsafe { psx_io::read16(base + 12) };
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
    Voice::key_on(voice.mask());
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
            let nibble = if (index % 28) * 2 < 28 { HIGH_NIBBLE } else { LOW_NIBBLE };
            let byte = at + 2 + sample / 2;
            if sample % 2 == 0 { small[byte] |= nibble } else { small[byte] |= nibble << 4 }
        }
    }
    spu::upload_adpcm(SpuAddr::new(SPU_SMALL_ADDR), &small);
}

/// `half_period_blocks == 1` gives one square cycle per 28 samples (1575 Hz
/// at unity pitch); 2 gives one cycle per 56 samples, an octave down.
fn build_square(table: &mut [u8], half_period_blocks: usize) {
    let period = 28 * half_period_blocks;
    for block in 0..TABLE_BLOCKS {
        let at = block * 16;
        table[at] = TONE_SHIFT; // filter 0
        // Loop-start on the first block, END+REPEAT on the LAST, nothing
        // in between. With only two blocks "not first" and "last" were
        // the same thing; at 64 they are not, and marking every middle
        // block as END would have ended the sample after one of them.
        table[at + 1] = if block == 0 {
            0x04
        } else if block == TABLE_BLOCKS - 1 {
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
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0000);
        psx_io::write16(SPU_TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0004);
        let cnt = psx_io::read16(SPU_CNT);
        psx_io::write16(SPU_CNT, (cnt & !0x0030) | 0x0010); // manual write
        for &word in words {
            psx_io::write16(SPU_TRANSFER_DATA, word as u16);
            psx_io::write16(SPU_TRANSFER_DATA, (word >> 16) as u16);
        }
        psx_io::write16(SPU_CNT, cnt & !0x0030);
        // 0x0004, never 0: see the note in pio_read.
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0004);
    }
}

/// Read back through the same port. If this and the DMA path disagree,
/// that difference is the SPU's read pipeline.
fn pio_read(addr: u32, out: &mut [u32]) {
    unsafe {
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0000);
        psx_io::write16(SPU_TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0004);
        let cnt = psx_io::read16(SPU_CNT);
        psx_io::write16(SPU_CNT, (cnt & !0x0030) | 0x0030); // manual read
        for word in out.iter_mut() {
            let lo = psx_io::read16(SPU_TRANSFER_DATA) as u32;
            let hi = psx_io::read16(SPU_TRANSFER_DATA) as u32;
            *word = lo | (hi << 16);
        }
        psx_io::write16(SPU_CNT, cnt & !0x0030);
        psx_io::write16(SPU_TRANSFER_CTRL, 0x0000);
    }
}

fn ram_label(stage: u8) -> &'static str {
    match stage {
        0 => "DMA/DMA",
        1 => "PIO/DMA",
        2 => "PIO/PIO",
        3 => "DMA/PIO",
        4 => "HIGHADR",
        5 => "SHORT",
        6 => "TWICE",
        _ => "?",
    }
}

fn ram_description(stage: u8) -> &'static str {
    match stage {
        0 => "THE SDK PATH: DMA IN, DMA OUT",
        1 => "CPU WRITES IN, DMA OUT",
        2 => "CPU WRITES IN, CPU READS OUT",
        3 => "DMA IN, CPU READS OUT",
        4 => "SAME TEST FAR UP SPU RAM",
        5 => "ONE ADPCM BLOCK ONLY",
        6 => "READ TWICE: IS THE READER STABLE?",
        _ => "?",
    }
}

fn tone_label(segment: u8) -> &'static str {
    match segment {
        0 => "SYNC",
        1 => "P 0400",
        2 => "P 0800",
        3 => "P 1000",
        4 => "P 2000",
        5 => "P 3000",
        6 => "P 3FFF",
        7 => "P 5000",
        8 => "HOLD",
        9 => "REKEY",
        10 => "ADDRSWAP",
        11 => "VOICES",
        12 => "NOISE",
        13 => "REPEXPL",
        14 => "SMALLTAB",
        15 => "PARKED",
        16 => "UNPARKED",
        17 => "ENDXBIT",
        18 => "ENVZERO",
        _ => "?",
    }
}

fn tone_description(segment: u8) -> &'static str {
    match segment {
        0 => "THREE BURSTS: FIND T=0 IN THE AUDIO",
        1..=6 => "PITCH LADDER ON ONE LOOPED TABLE",
        7 => "OVER-RANGE PITCH: DOES SILICON CLAMP?",
        8 => "ONE LOOP HELD 5S: DOES IT STAY CLEAN?",
        9 => "RE-KEY EVERY 4 FRAMES (PICO-8 RATE)",
        10 => "START ADDR CHANGED WITHOUT KEY ON",
        11 => "1 THEN 4 THEN 8 VOICES TOGETHER",
        12 => "NOISE MODE: SILICON'S OWN LFSR",
        13 => "SAME TABLE, REPEAT ADDR SET BY HAND",
        14 => "A 32-BYTE TABLE, SAME UPLOAD PATH",
        _ => "?",
    }
}

/// What a recording should show if silicon behaves. On screen so a wrong
/// answer is audible to the operator without waiting for the decode.
fn tone_expectation(segment: u8) -> &'static str {
    match segment {
        0 => "3 BEEPS 1575HZ",
        1 => "394 HZ",
        2 => "788 HZ",
        3 => "1575 HZ",
        4 => "3150 HZ",
        5 => "4725 HZ",
        6 => "6300 HZ",
        7 => "6300 CLAMPED / 7875 NOT",
        8 => "1575 STEADY, NO CLICKS",
        9 => "1575, NO CLICK PER KEY",
        10 => "1575 THEN 788 AT THE SWAP",
        11 => "1575, LOUDER IN 3 STEPS",
        12 => "HISS, NO TONE",
        13 => "1575 HZ IF THE FLAG WAS THE FAULT",
        14 => "1575 HZ IF SIZE NEVER MATTERED",
        _ => "?",
    }
}

fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    target[*len..*len + bytes.len()].copy_from_slice(bytes);
    *len += bytes.len();
}

fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = 0usize;
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let word = (b0 << 16) | (b1 << 8) | b2;
        output[out] = TABLE[(word >> 18) as usize & 63];
        output[out + 1] = TABLE[(word >> 12) as usize & 63];
        output[out + 2] = if chunk.len() > 1 {
            TABLE[(word >> 6) as usize & 63]
        } else {
            b'='
        };
        output[out + 3] = if chunk.len() > 2 {
            TABLE[word as usize & 63]
        } else {
            b'='
        };
        out += 4;
    }
    out
}
