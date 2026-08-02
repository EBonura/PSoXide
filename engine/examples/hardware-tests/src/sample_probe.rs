//! SB1: the UI sample end/loop probe.
//!
//! Built for one bug: the demo-disc launcher's browse blip repeats
//! aggressively on the console. The blip path is `configure_sample` +
//! `Adsr::sample()` + one `key_on` and no `key_off`, which means the
//! voice stops ONLY if the sample's own ADPCM end flags stop it: an
//! END+MUTE terminator silences the voice, an END+REPEAT terminator
//! loops it forever at full sustain. An emulator that skips the flag
//! semantics plays a polite single blip either way, so only silicon can
//! answer -- and this probe makes it answer in numbers rather than ears.
//!
//! Stage 0 never plays a note: it uploads the real launcher browse blip
//! (ui_beep, a byte-identical include of the shipped asset) and audits
//! its block flags plus an SPU RAM readback, so the static question
//! "what does the terminator actually say" is settled first. Four keyed
//! stages then trace the dynamic story -- envelope level and ENDX at
//! eight checkpoints over five seconds each:
//!
//!   BEEP ONESHOT   the launcher path exactly, one key_on
//!   BEEP MASH      key_on at 0/10/20/30, a fast browse
//!   BEEP KEYOFF    one key_on, key_off at frame 60
//!   BEEP PERC      Adsr::percussive() instead: the self-fading preset
//!   BEEP DTONE     Adsr::default_tone(): full play then ~100ms release
//!                  at the END flag -- the envelope the v0.8 sweep moved
//!                  every game one-shot to, measured here
//!
//! A looping sample reads as envelope pinned at max with ENDX never
//! set; a clean one-shot reads as ENDX set with the envelope at zero.
//! The KEYOFF stage measures how slowly `Adsr::sample()`'s release
//! actually is, and PERC shows what the fixed percussive preset does on
//! real silicon. Everything lands in one QR.

use psx_asset::Audio;
use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::photo::crc32;
use crate::{hex2, hex8, spu_dma_read};

/// The launcher's own cooked browse blip, byte for byte. The select
/// blip (pickup_coin) stays out: at 7 KiB it alone overflowed the
/// playtest boot-EXE budget, and the reported bug is the browse blip.
static BEEP_PSAU: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/ui_beep.psau");

const STAGE_COUNT: usize = 6;
const FIELD_COUNT: usize = 9;
/// Frames each stage runs. Stage 0 is the silent audit.
const STAGE_FRAMES: [u16; STAGE_COUNT] = [60, 300, 300, 300, 300, 300];
/// Frames within a stage at which the envelope/ENDX checkpoints sample.
const CHECKPOINTS: [u16; 8] = [2, 8, 16, 32, 64, 120, 200, 280];

/// SPU layout: the blip at the first free SPU address, like the SDK.
const SPU_SAMPLE_BASE: u32 = 0x1010;
/// The launcher's browse voice, mirrored exactly.
const BEEP_VOICE: u8 = 0;

const ENDX_LO: u32 = psx_io::spu::SPU_BASE + 0x19C;

/// Readback staging for the audit: ui_beep is 2 KiB of psau, so its
/// ADPCM fits with room to spare.
static mut READBACK: [u32; 1024] = [0; 1024];

const QR_VERSION: Version = Version::new(15);
const QR_SIZE: usize = 77;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 264;
const BASE64_LEN: usize = 352;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

#[derive(Copy, Clone)]
struct StageRecord {
    fields: [u32; FIELD_COUNT],
}

impl StageRecord {
    const fn empty() -> Self {
        Self {
            fields: [0; FIELD_COUNT],
        }
    }
}

/// Static audit of one sample's ADPCM blocks.
#[derive(Copy, Clone, Default)]
struct FlagAudit {
    blocks: u16,
    /// OR of every block's flags byte.
    or_flags: u8,
    /// The final block's flags byte: the terminator the voice will obey.
    last_flags: u8,
    /// Index of the first block with the END bit, or 0xFFFF if none.
    first_end: u16,
    /// Count of blocks with the LOOP-START bit.
    loop_starts: u8,
}

impl FlagAudit {
    fn of(adpcm: &[u8]) -> Self {
        let mut audit = Self {
            blocks: (adpcm.len() / 16) as u16,
            first_end: 0xFFFF,
            ..Self::default()
        };
        for (index, block) in adpcm.chunks_exact(16).enumerate() {
            let flags = block[1];
            audit.or_flags |= flags;
            audit.last_flags = flags;
            if flags & 0x01 != 0 && audit.first_end == 0xFFFF {
                audit.first_end = index as u16;
            }
            if flags & 0x04 != 0 {
                audit.loop_starts = audit.loop_starts.saturating_add(1);
            }
        }
        audit
    }

    fn packed_summary(self) -> u32 {
        ((self.blocks as u32) << 16) | ((self.or_flags as u32) << 8) | self.last_flags as u32
    }

    fn packed_detail(self) -> u32 {
        ((self.first_end as u32) << 16) | ((self.loop_starts as u32) << 8)
    }
}

pub(crate) struct SampleProbe {
    stage: u8,
    stage_frame: u16,
    complete: bool,
    run: u8,
    records: [StageRecord; STAGE_COUNT],
    beep_addr: u32,
    beep_rate: u32,
    beep_audit: FlagAudit,
    beep_readback_fnv: u32,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl SampleProbe {
    pub(crate) const fn new() -> Self {
        Self {
            stage: 0,
            stage_frame: 0,
            complete: false,
            run: 0,
            records: [StageRecord::empty(); STAGE_COUNT],
            beep_addr: 0,
            beep_rate: 0,
            beep_audit: FlagAudit {
                blocks: 0,
                or_flags: 0,
                last_flags: 0,
                first_end: 0,
                loop_starts: 0,
            },
            beep_readback_fnv: 0,
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn start(&mut self) {
        self.stage = 0;
        self.stage_frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.records = [StageRecord::empty(); STAGE_COUNT];
        self.qr_size = 0;
        spu::init();
        spu::set_main_volume(Volume::MAX, Volume::MAX);
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::println("hardware-tests: sb1 begin ui-sample probe");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    pub(crate) fn update(&mut self, tick: u32) {
        if self.complete {
            return;
        }
        let stage = self.stage as usize;

        // Per-stage key scripts, frame-exact so runs are comparable.
        match self.stage {
            2 => {
                // A browse mash: retrigger every ten frames, four times.
                if matches!(self.stage_frame, 10 | 20 | 30) {
                    Voice::key_on(Voice::new(BEEP_VOICE).mask());
                }
            }
            3 => {
                if self.stage_frame == 60 {
                    Voice::key_off(Voice::new(BEEP_VOICE).mask());
                }
            }
            _ => {}
        }

        if let Some(slot) = CHECKPOINTS.iter().position(|&f| f == self.stage_frame) {
            let voice = BEEP_VOICE;
            let base = psx_io::spu::SPU_BASE + voice as u32 * 16;
            let env = unsafe { psx_io::read16(base + 12) };
            let endx = unsafe { psx_io::read16(ENDX_LO) };
            let endx_bit = (endx >> BEEP_VOICE) & 1;
            self.records[stage].fields[1 + slot] = ((env as u32) << 16) | endx_bit as u32;
        }

        self.stage_frame = self.stage_frame.saturating_add(1);
        if self.stage_frame < STAGE_FRAMES[stage] {
            return;
        }

        // Stage over: log its verdict line, silence, and move on.
        let record = &self.records[stage];
        tty::print("hardware-tests: sb1 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" last=");
        tty::println(hex8(record.fields[FIELD_COUNT - 1]).digits());

        Voice::key_off(Voice::new(BEEP_VOICE).mask());
        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            self.records[self.stage as usize].fields[0] =
                ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF);
            self.apply_stage();
        } else {
            self.complete = true;
            self.encode_qr();
            self.print_payload();
        }
    }

    fn apply_stage(&mut self) {
        match self.stage {
            0 => self.audit(),
            1 | 2 | 3 => self.key(BEEP_VOICE, self.beep_addr, self.beep_rate, Adsr::sample()),
            4 => self.key(BEEP_VOICE, self.beep_addr, self.beep_rate, Adsr::percussive()),
            5 => self.key(BEEP_VOICE, self.beep_addr, self.beep_rate, Adsr::default_tone()),
            _ => unreachable!(),
        }
        tty::print("hardware-tests: sb1 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" label=");
        tty::println(stage_label(self.stage));
    }

    /// Upload both launcher samples and settle the static questions:
    /// what the terminator flags say, and whether SPU RAM holds the
    /// bytes we think it does.
    fn audit(&mut self) {
        let beep = Audio::from_bytes(BEEP_PSAU).expect("cooked ui_beep psau");
        let beep_bytes = beep.adpcm_bytes();
        self.beep_rate = beep.sample_rate_hz();
        self.beep_audit = FlagAudit::of(beep_bytes);

        self.beep_addr = SPU_SAMPLE_BASE;
        spu::upload_adpcm(SpuAddr::new(self.beep_addr), beep_bytes);

        // Read the beep back out of SPU RAM: a mismatch here would blame
        // the upload rather than the flags, so it has to be on record.
        let words = beep_bytes.len().div_ceil(4).min(1024);
        let readback = unsafe {
            core::slice::from_raw_parts_mut(core::ptr::addr_of_mut!(READBACK) as *mut u32, words)
        };
        spu_dma_read(self.beep_addr, readback);
        let mut hash = 0x811C_9DC5u32;
        for &word in readback.iter() {
            hash = (hash ^ word).wrapping_mul(0x0100_0193);
        }
        self.beep_readback_fnv = hash;

        tty::print("hardware-tests: sb1 beep blocks=");
        tty::println(hex8(self.beep_audit.packed_summary()).digits());
    }

    fn key(&self, voice: u8, addr: u32, rate: u32, adsr: Adsr) {
        let voice = Voice::new(voice);
        // The launcher's exact volume, so the trace is its trace.
        voice.configure_sample(SpuAddr::new(addr), rate, Volume::linear(1, 14), adsr);
        Voice::key_on(voice.mask());
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let len = self.write_binary(&mut binary);
        let crc = crc32(&binary[..len]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        let payload_len = base64_encode(&binary, &mut payload);
        assert!(payload_len == BASE64_LEN, "SB1 Base64 layout drift");
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"SB1/");
        append(&mut text, &mut text_len, &payload);
        append(&mut text, &mut text_len, b"/C:");
        append(
            &mut text,
            &mut text_len,
            hex8(self.binary_crc).digits().as_bytes(),
        );
        let encoded = unsafe { core::str::from_utf8_unchecked(&text[..text_len]) };
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

    /// Everything the decode needs, CRC last. Bytes past the payload up
    /// to `BINARY_LEN - 4` stay zero.
    fn write_binary(&self, binary: &mut [u8; BINARY_LEN]) -> usize {
        let mut at = 0usize;
        let mut push = |binary: &mut [u8; BINARY_LEN], bytes: &[u8]| {
            binary[at..at + bytes.len()].copy_from_slice(bytes);
            at += bytes.len();
            at
        };
        push(binary, b"SB1B");
        push(binary, &[2, STAGE_COUNT as u8, FIELD_COUNT as u8, self.run]);
        push(binary, &self.beep_audit.packed_summary().to_le_bytes());
        push(binary, &self.beep_audit.packed_detail().to_le_bytes());
        push(binary, &self.beep_rate.to_le_bytes());
        push(binary, &self.beep_readback_fnv.to_le_bytes());
        let mut len = 0;
        for record in self.records {
            for value in record.fields {
                len = push(binary, &value.to_le_bytes());
            }
        }
        assert!(len <= BINARY_LEN - 4, "SB1 binary layout drift");
        len
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        self.write_binary(&mut binary);
        binary[BINARY_LEN - 4..].copy_from_slice(&self.binary_crc.to_le_bytes());
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: sb1 SB1/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "UI SAMPLE END/LOOP PROBE SB1", (255, 232, 128));
        font.draw_text(8, 20, "AUTOMATIC - RECORD COMPLETE RUN", (150, 170, 200));
        if !self.complete {
            let stage = self.stage as usize;
            font.draw_text(8, 42, "STAGE", (140, 160, 190));
            font.draw_text(64, 42, hex2(self.stage).as_str(), (232, 236, 244));
            font.draw_text(88, 42, stage_label(self.stage), (96, 200, 255));
            font.draw_text(8, 58, stage_description(self.stage), (220, 224, 230));
            font.draw_text(8, 76, "TIME LEFT", (140, 160, 190));
            let remaining = STAGE_FRAMES[stage].saturating_sub(self.stage_frame);
            font.draw_text(88, 76, hex8(remaining as u32).digits(), (232, 236, 244));
            font.draw_text(8, 108, "EACH KEYED STAGE = ONE SHORT BLIP", (112, 136, 170));
            font.draw_text(8, 124, "A CONSTANT BUZZ = SAMPLE LOOPS ON", (255, 128, 96));
            font.draw_text(8, 140, "HARDWARE: THE LAUNCHER BUG, CAUGHT", (255, 128, 96));
            font.draw_text(8, 172, "QR APPEARS AFTER ALL 6 STAGES", (112, 136, 170));
            return;
        }

        font.draw_text(8, 36, "COMPLETE - HOLD CAMERA STEADY ON QR", (96, 240, 128));
        font.draw_text(8, 226, "X RERUN", (150, 170, 200));
        if self.qr_size as usize != QR_SIZE {
            font.draw_text(88, 112, "QR ENCODE FAILED", (255, 96, 96));
            return;
        }
        let total = (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE;
        let left = (320 - total) / 2;
        let top = 50;
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

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
    }
}

fn stage_label(stage: u8) -> &'static str {
    match stage {
        0 => "FLAG AUDIT",
        1 => "BEEP ONESHOT",
        2 => "BEEP MASH",
        3 => "BEEP KEYOFF",
        4 => "BEEP PERC",
        5 => "BEEP DTONE",
        _ => "?",
    }
}

fn stage_description(stage: u8) -> &'static str {
    match stage {
        0 => "READ TERMINATOR FLAGS + SPU READBACK",
        1 => "LAUNCHER PATH: KEY ON, NEVER OFF",
        2 => "RETRIGGER AT 0/10/20/30: FAST BROWSE",
        3 => "KEY OFF AT 60: MEASURE THE RELEASE",
        4 => "PERCUSSIVE PRESET: SELF-FADING?",
        5 => "DEFAULT TONE: FULL PLAY + FAST RELEASE?",
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
