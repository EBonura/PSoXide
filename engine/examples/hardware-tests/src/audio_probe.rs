//! Automatic real-console CD -> SPU audio-path discriminator.
//!
//! The five fixed-duration stages reproduce the game state that produced the
//! real-PS1 noise, then change one routing control at a time.  A compact PA1
//! QR payload preserves the SPU CD capture-buffer measurements so the OBS
//! recording and internal console state can be compared without transcription.

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_io::{cdrom, dma};
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, CdVolume, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, spu_dma_read};

const STAGE_COUNT: usize = 5;
const STAGE_FRAMES: u16 = 180;
const CAPTURE_FRAME: u16 = 120;
const CDTEST_LBA: u32 = 524;
const SECTOR_WORDS: usize = 2048 / 4;
const CAPTURE_WORDS: usize = 0x800 / 4;
const CAPTURE_HALF_WORDS: usize = 0x400 / 4;
const FIELD_COUNT: usize = 10;

// Filter 0 / shift 0, loop start+end+repeat, alternating maximum ADPCM
// nibbles. Voice volume limits the resulting square-like reference to a safe
// level while keeping it plainly visible in an OBS waveform.
const CALIBRATION_TONE: [u8; 16] = [
    0x00, 0x07, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
];

const CD_STATUS: u32 = cdrom::BASE;
const CD_REQUEST_IRQ: u32 = cdrom::BASE + 3;
const CD_STATUS_DATA_READY: u8 = 1 << 6;
const CD_IRQ_DATA_READY: u8 = 1;

const SPU_CD_VOL_LEFT: u32 = psx_io::spu::SPU_BASE + 0x1B0;
const SPU_CD_VOL_RIGHT: u32 = psx_io::spu::SPU_BASE + 0x1B2;

const QR_VERSION: Version = Version::new(15);
const QR_SIZE: usize = 77;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;

const BINARY_LEN: usize = 220;
const BASE64_LEN: usize = 296;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

#[derive(Copy, Clone)]
pub(crate) struct AudioStageRecord {
    fields: [u32; FIELD_COUNT],
}

impl AudioStageRecord {
    const fn empty() -> Self {
        Self {
            fields: [0; FIELD_COUNT],
        }
    }
}

pub(crate) struct AudioProbe {
    stage: u8,
    stage_frame: u16,
    complete: bool,
    run: u8,
    command_state: u8,
    sectors: u16,
    records: [AudioStageRecord; STAGE_COUNT],
    sector_buffer: [u32; SECTOR_WORDS],
    capture_buffer: [u32; CAPTURE_WORDS],
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl AudioProbe {
    pub(crate) const fn new() -> Self {
        Self {
            stage: 0,
            stage_frame: 0,
            complete: false,
            run: 0,
            command_state: 0,
            sectors: 0,
            records: [AudioStageRecord::empty(); STAGE_COUNT],
            sector_buffer: [0; SECTOR_WORDS],
            capture_buffer: [0; CAPTURE_WORDS],
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
        self.command_state = 0;
        self.sectors = 0;
        self.records = [AudioStageRecord::empty(); STAGE_COUNT];
        self.qr_size = 0;

        // Establish the exact game-side state explicitly instead of inheriting
        // whatever the preceding conformance tests left in the SPU.
        spu::init();
        spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
        spu::enable_cd_audio(false);
        let tone_addr = SpuAddr::new(0x2000);
        psx_spu::upload_adpcm(tone_addr, &CALIBRATION_TONE);
        Voice::V0.set_volume(Volume::linear(1, 8), Volume::linear(1, 8));
        Voice::V0.set_pitch(Pitch::UNITY);
        Voice::V0.set_start_addr(tone_addr);
        Voice::V0.set_adsr(Adsr::sample());
        let _ = cdrom::try_pause_until_complete(200_000);
        let _ = cdrom::try_demute(200_000);
        self.apply_stage();
        tty::println("hardware-tests: pa1 begin automatic CD/SPU probe");
    }

    pub(crate) fn update(&mut self, tick: u32) {
        if self.complete {
            return;
        }

        if self.stage == 0 {
            if self.stage_frame == 15 {
                Voice::key_on(Voice::V0.mask());
            } else if self.stage_frame == 45 {
                Voice::key_off(Voice::V0.mask());
                Voice::V0.set_volume(Volume::SILENCE, Volume::SILENCE);
            }
        }

        if (1..=3).contains(&self.stage) {
            self.service_cd_sector();
        }

        if self.stage_frame == CAPTURE_FRAME {
            self.capture_stage(tick);
        }

        self.stage_frame = self.stage_frame.saturating_add(1);
        if self.stage_frame < STAGE_FRAMES {
            return;
        }

        self.stage_frame = 0;
        if self.stage as usize + 1 < STAGE_COUNT {
            self.stage += 1;
            self.apply_stage();
        } else {
            self.complete = true;
            self.encode_qr();
            self.print_payload();
        }
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    fn apply_stage(&mut self) {
        self.command_state = match self.stage {
            0 => {
                spu::enable_cd_audio(false);
                let _ = cdrom::try_demute(200_000);
                1
            }
            1 => {
                spu::enable_cd_audio(false);
                let demute = cdrom::try_demute(200_000).is_some();
                let mode = cdrom::try_set_mode(cdrom::MODE_DOUBLE_SPEED, 200_000).is_some();
                let loc = cdrom::try_set_loc_lba(CDTEST_LBA, 200_000).is_some();
                let read = cdrom::try_read_n(200_000).is_some();
                demute as u8 | ((mode as u8) << 1) | ((loc as u8) << 2) | ((read as u8) << 3)
            }
            2 => {
                spu::enable_cd_audio(false);
                cdrom::try_mute(200_000).is_some() as u8
            }
            3 => {
                let demute = cdrom::try_demute(200_000).is_some();
                spu::enable_cd_audio(true);
                demute as u8
            }
            _ => {
                spu::enable_cd_audio(false);
                let paused = cdrom::try_pause_until_complete(200_000);
                let demute = cdrom::try_demute(200_000).is_some();
                paused as u8 | ((demute as u8) << 1)
            }
        };
        tty::print("hardware-tests: pa1 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" label=");
        tty::println(stage_label(self.stage));
    }

    fn service_cd_sector(&mut self) {
        let irq = cd_irq_flag();
        let status = unsafe { psx_io::read8(CD_STATUS) };
        if irq != CD_IRQ_DATA_READY && status & CD_STATUS_DATA_READY == 0 {
            return;
        }

        cd_select(0);
        unsafe { psx_io::write8(CD_REQUEST_IRQ, 0x80) };
        dma::enable_channel(dma::Channel::Cdrom);
        dma::set_madr(dma::Channel::Cdrom, self.sector_buffer.as_mut_ptr() as u32);
        dma::set_bcr_manual(dma::Channel::Cdrom, SECTOR_WORDS as u16);
        dma::set_chcr(dma::Channel::Cdrom, 0x1140_0100);
        let mut guard = 0u32;
        while dma::is_busy(dma::Channel::Cdrom) && guard < 1_000_000 {
            guard += 1;
        }
        psx_io::irq::ack(1 << psx_io::irq::source::DMA);
        cd_select(1);
        unsafe { psx_io::write8(CD_REQUEST_IRQ, CD_IRQ_DATA_READY) };
        psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
        cd_select(0);
        self.sectors = self.sectors.saturating_add(1);
    }

    fn capture_stage(&mut self, tick: u32) {
        // One contiguous DMA read avoids introducing an artificial boundary
        // between the adjacent 0x400-byte CD-L and CD-R capture rings.
        spu_dma_read(0, &mut self.capture_buffer);
        let left = &self.capture_buffer[..CAPTURE_HALF_WORDS];
        let right = &self.capture_buffer[CAPTURE_HALF_WORDS..];
        let left_stats = capture_stats(left);
        let right_stats = capture_stats(right);
        let spucnt = unsafe { psx_io::read16(psx_io::spu::SPUCNT) };
        let spustat = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) };
        let cd_left = unsafe { psx_io::read16(SPU_CD_VOL_LEFT) };
        let cd_right = unsafe { psx_io::read16(SPU_CD_VOL_RIGHT) };
        let cd_status = unsafe { psx_io::read8(CD_STATUS) };
        let irq = cd_irq_flag();
        self.records[self.stage as usize].fields = [
            ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF),
            ((spucnt as u32) << 16) | spustat as u32,
            ((cd_left as u32) << 16) | cd_right as u32,
            ((cd_status as u32) << 24)
                | (((irq & 0x0F) as u32) << 20)
                | (((self.command_state & 0x0F) as u32) << 16)
                | self.sectors as u32,
            left_stats.hash,
            right_stats.hash,
            ((left_stats.nonzero as u32) << 16) | right_stats.nonzero as u32,
            ((left_stats.peak as u32) << 16) | right_stats.peak as u32,
            left_stats.energy,
            right_stats.energy,
        ];
        tty::print("hardware-tests: pa1 sample stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" lhash=");
        tty::print(hex8(left_stats.hash).digits());
        tty::print(" rhash=");
        tty::println(hex8(right_stats.hash).digits());
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        out.push_bytes(b"PA1B");
        out.push_u8(1);
        out.push_u8(STAGE_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(CDTEST_LBA);
        out.push_u32(STAGE_FRAMES as u32);
        for record in self.records {
            for value in record.fields {
                out.push_u32(value);
            }
        }
        let crc = crc32(out.bytes());
        out.push_u32(crc);
        let len = out.len();
        drop(out);
        assert!(len == BINARY_LEN, "PA1 binary layout drift");
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        let payload_len = base64_encode(&binary, &mut payload);
        assert!(payload_len == BASE64_LEN, "PA1 Base64 layout drift");
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"PA1/");
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
            QrCodeEcc::Low,
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
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        out.push_bytes(b"PA1B");
        out.push_u8(1);
        out.push_u8(STAGE_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(CDTEST_LBA);
        out.push_u32(STAGE_FRAMES as u32);
        for record in self.records {
            for value in record.fields {
                out.push_u32(value);
            }
        }
        out.push_u32(self.binary_crc);
        drop(out);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: pa1 PA1/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "CD -> SPU AUDIO CONFORMANCE", (255, 232, 128));
        font.draw_text(8, 20, "AUTOMATIC - RECORD COMPLETE RUN", (150, 170, 200));
        if !self.complete {
            font.draw_text(8, 42, "STAGE", (140, 160, 190));
            font.draw_text(64, 42, hex2(self.stage).as_str(), (232, 236, 244));
            font.draw_text(88, 42, stage_label(self.stage), stage_color(self.stage));
            font.draw_text(8, 58, stage_description(self.stage), (220, 224, 230));
            font.draw_text(8, 76, "TIME LEFT", (140, 160, 190));
            let remaining = STAGE_FRAMES.saturating_sub(self.stage_frame);
            font.draw_text(88, 76, hex8(remaining as u32).digits(), (232, 236, 244));
            font.draw_text(8, 94, "SECTORS", (140, 160, 190));
            font.draw_text(88, 94, hex8(self.sectors as u32).digits(), (232, 236, 244));
            font.draw_text(
                8,
                124,
                "A CALIBRATION BEEP PLAYS IN STAGE 0",
                (112, 136, 170),
            );
            font.draw_text(8, 140, "DO NOT CHANGE VOLUME OR STOP OBS", (255, 216, 96));
            font.draw_text(
                8,
                156,
                "QR APPEARS WHEN ALL 5 STAGES FINISH",
                (112, 136, 170),
            );
            return;
        }

        font.draw_text(8, 36, "COMPLETE - HOLD CAMERA STEADY ON QR", (96, 240, 128));
        font.draw_text(
            8,
            226,
            "X RERUN  DOWN OTHER HARDWARE TESTS",
            (150, 170, 200),
        );
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

struct CaptureStats {
    hash: u32,
    nonzero: u16,
    peak: u16,
    energy: u32,
}

fn capture_stats(words: &[u32]) -> CaptureStats {
    let mut hash = 0x811C_9DC5u32;
    let mut nonzero = 0u16;
    let mut peak = 0u16;
    let mut energy = 0u32;
    for &word in words {
        for raw in [(word & 0xFFFF) as u16, (word >> 16) as u16] {
            hash = (hash ^ raw as u32).wrapping_mul(0x0100_0193);
            let signed = raw as i16 as i32;
            let magnitude = signed.unsigned_abs().min(0xFFFF) as u16;
            if raw != 0 {
                nonzero = nonzero.saturating_add(1);
            }
            peak = peak.max(magnitude);
            energy = energy.saturating_add(magnitude as u32);
        }
    }
    CaptureStats {
        hash,
        nonzero,
        peak,
        energy,
    }
}

fn stage_label(stage: u8) -> &'static str {
    match stage {
        0 => "IDLE + CALIBRATION",
        1 => "READN / GAME ROUTE OFF",
        2 => "READN / CD CONTROLLER MUTED",
        3 => "READN / SPU CD ROUTE ON",
        _ => "PAUSED / POST-READ",
    }
}

fn stage_description(stage: u8) -> &'static str {
    match stage {
        0 => "DRIVE IDLE; SPU VOICE PROVES OBS AUDIO",
        1 => "EXACT HL STATE: FULL CD VOL, SPUCNT BIT0=0",
        2 => "ONLY CD-ROM MUTE CHANGES",
        3 => "DEMUTED; ONLY SPUCNT CD MIX BIT CHANGES",
        _ => "READ PAUSED; ROUTE RESTORED OFF",
    }
}

fn stage_color(stage: u8) -> (u8, u8, u8) {
    match stage {
        1 => (255, 128, 96),
        2 => (255, 216, 96),
        3 => (160, 128, 255),
        _ => (96, 240, 128),
    }
}

fn cd_select(index: u8) {
    unsafe { psx_io::write8(CD_STATUS, index & 3) };
}

fn cd_irq_flag() -> u8 {
    cd_select(1);
    let flag = unsafe { psx_io::read8(CD_REQUEST_IRQ) } & 0x1F;
    cd_select(0);
    flag
}

fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let end = *len + bytes.len();
    target[*len..end].copy_from_slice(bytes);
    *len = end;
}

struct BinaryBuffer<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> BinaryBuffer<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push_u8(&mut self, value: u8) {
        assert!(self.len < self.bytes.len(), "PA1 binary overflow");
        self.bytes[self.len] = value;
        self.len += 1;
    }

    fn push_u32(&mut self, value: u32) {
        self.push_bytes(&value.to_le_bytes());
    }

    fn push_bytes(&mut self, values: &[u8]) {
        for &value in values {
            self.push_u8(value);
        }
    }
}

fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut source = 0usize;
    let mut target = 0usize;
    while source < input.len() {
        let remaining = input.len() - source;
        let a = input[source];
        let b = if remaining > 1 { input[source + 1] } else { 0 };
        let c = if remaining > 2 { input[source + 2] } else { 0 };
        output[target] = ALPHABET[(a >> 2) as usize];
        output[target + 1] = ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize];
        output[target + 2] = if remaining > 1 {
            ALPHABET[(((b & 0x0F) << 2) | (c >> 6)) as usize]
        } else {
            b'='
        };
        output[target + 3] = if remaining > 2 {
            ALPHABET[(c & 0x3F) as usize]
        } else {
            b'='
        };
        source += remaining.min(3);
        target += 4;
    }
    target
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        let mut bit = 0;
        while bit < 8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            bit += 1;
        }
    }
    !crc
}
