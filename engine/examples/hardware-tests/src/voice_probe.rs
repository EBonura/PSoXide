//! PA2: automatic reproduction of hl-psx's real gameplay SPU voice path.
//!
//! The fixture preserves the exact Hazard Course bank dimensions and upload
//! order but replaces copyrighted speech with deterministic synthetic ADPCM.
//! Sample 0 contains a short marker followed by silence; sample 1 is a loud
//! overrun guard.  Hearing that guard means voice 15 failed to stop at sample
//! 0's final ADPCM block, matching the persistent-noise failure seen on PS1.

use psx_asset::Audio;
use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_io::dma;
use psx_rt::tty;
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, spu_dma_read};

include!(concat!(env!("OUT_DIR"), "/pa2_meta.rs"));

#[repr(C, align(4))]
struct PackStaging([u32; 92_160]); // 368,640 bytes; t0a0 is 368,392 bytes.

static mut PACK_STAGING: PackStaging = PackStaging([0; 92_160]);

const STAGE_COUNT: usize = 6;
const FIELD_COUNT: usize = 10;
const STAGE_FRAMES: [u16; STAGE_COUNT] = [180, 180, 180, 360, 180, 360];
const CAPTURE_FRAMES: [u16; STAGE_COUNT] = [120, 120, 120, 300, 120, 300];
const SPU_SAMPLE_BASE: u32 = 0x1010;
const DIALOGUE_VOICE: u8 = 15;
const CALIBRATION_ADDR: u32 = 0x7FF00;
const TAIL_BYTES: usize = 64;

const CALIBRATION_TONE: [u8; 16] = [
    0x00, 0x07, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
];

const QR_VERSION: Version = Version::new(15);
const QR_SIZE: usize = 77;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 264;
const BASE64_LEN: usize = 352;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

const VOICE_BASE: u32 = psx_io::spu::SPU_BASE + DIALOGUE_VOICE as u32 * 16;
const ENDX_LO: u32 = psx_io::spu::SPU_BASE + 0x19C;
const ENDX_HI: u32 = psx_io::spu::SPU_BASE + 0x19E;

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

#[derive(Copy, Clone, Default)]
struct UploadTiming {
    max_mode_polls: u16,
    max_drain_polls: u16,
}

impl UploadTiming {
    fn merge(&mut self, other: Self) {
        self.max_mode_polls = self.max_mode_polls.max(other.max_mode_polls);
        self.max_drain_polls = self.max_drain_polls.max(other.max_drain_polls);
    }

    fn packed(self) -> u32 {
        ((self.max_mode_polls as u32) << 16) | self.max_drain_polls as u32
    }
}

pub(crate) struct VoiceProbe {
    stage: u8,
    stage_frame: u16,
    complete: bool,
    run: u8,
    records: [StageRecord; STAGE_COUNT],
    target_addr: u32,
    target_rate: u32,
    target_len: u32,
    target_hash: u32,
    expected_tail_hash: u32,
    observed_tail_hash: u32,
    upload_timing: UploadTiming,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl VoiceProbe {
    pub(crate) const fn new() -> Self {
        Self {
            stage: 0,
            stage_frame: 0,
            complete: false,
            run: 0,
            records: [StageRecord::empty(); STAGE_COUNT],
            target_addr: 0,
            target_rate: 0,
            target_len: 0,
            target_hash: 0,
            expected_tail_hash: 0,
            observed_tail_hash: 0,
            upload_timing: UploadTiming {
                max_mode_polls: 0,
                max_drain_polls: 0,
            },
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
        self.target_addr = 0;
        self.target_rate = 0;
        self.target_len = 0;
        self.target_hash = 0;
        self.expected_tail_hash = 0;
        self.observed_tail_hash = 0;
        self.upload_timing = UploadTiming::default();
        self.qr_size = 0;
        spu::init();
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::println("hardware-tests: pa2 begin automatic voice-bank probe");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
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

        let stage = self.stage as usize;
        if self.stage_frame == CAPTURE_FRAMES[stage] {
            self.capture_stage(tick);
        }

        self.stage_frame = self.stage_frame.saturating_add(1);
        if self.stage_frame < STAGE_FRAMES[stage] {
            return;
        }

        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            self.apply_stage();
        } else {
            let voice = Voice::new(DIALOGUE_VOICE);
            voice.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(voice.mask());
            self.complete = true;
            self.encode_qr();
            self.print_payload();
        }
    }

    fn apply_stage(&mut self) {
        match self.stage {
            0 => {
                spu::upload_adpcm(SpuAddr::new(CALIBRATION_ADDR), &CALIBRATION_TONE);
                Voice::V0.set_volume(Volume::linear(1, 8), Volume::linear(1, 8));
                Voice::V0.set_pitch(Pitch::UNITY);
                Voice::V0.set_start_addr(SpuAddr::new(CALIBRATION_ADDR));
                Voice::V0.set_adsr(Adsr::sample());
            }
            1 => self.load_banks(false),
            2 => self.play_target(),
            3 | 5 => {}
            4 => {
                let voice = Voice::new(DIALOGUE_VOICE);
                voice.set_volume(Volume::SILENCE, Volume::SILENCE);
                Voice::key_off(voice.mask());
                self.load_banks(true);
                self.play_target();
            }
            _ => unreachable!(),
        }
        tty::print("hardware-tests: pa2 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" label=");
        tty::println(stage_label(self.stage));
    }

    fn load_banks(&mut self, safe: bool) {
        spu::init();
        spu::enable_cd_audio(false);
        self.upload_timing = UploadTiming::default();
        let mut next = SPU_SAMPLE_BASE;
        let staging = unsafe {
            core::slice::from_raw_parts_mut(
                PACK_STAGING.0.as_mut_ptr() as *mut u8,
                PACK_STAGING.0.len() * 4,
            )
        };
        let core_len = build_pack(PA2_CORE_LAYOUT, None, staging);
        next = self.upload_pack(&staging[..core_len], next, safe, false);
        let map_len = build_pack(PA2_MAP_LAYOUT, Some(1), staging);
        let end = self.upload_pack(&staging[..map_len], next, safe, true);
        assert!(end <= 512 * 1024, "PA2 fixture exceeds SPU RAM");
        assert!(self.target_len >= TAIL_BYTES as u32, "PA2 target too short");

        let source = target_bytes(&staging[..map_len]).expect("PA2 map target");
        self.target_hash = fnv16_bytes(source);
        let tail = &source[source.len() - TAIL_BYTES..];
        self.expected_tail_hash = fnv16_bytes(tail);
        let mut back = [0u32; TAIL_BYTES / 4];
        spu_dma_read(
            self.target_addr + self.target_len - TAIL_BYTES as u32,
            &mut back,
        );
        self.observed_tail_hash = fnv32_words(&back);
    }

    fn upload_pack(&mut self, pack: &[u8], mut next: u32, safe: bool, map: bool) -> u32 {
        assert!(pack.len() >= 8 && &pack[..4] == b"HSFX", "PA2 HSFX fixture");
        let count = rd_u32(pack, 4) as usize;
        for index in 0..count {
            let offset = rd_u32(pack, 8 + index * 8) as usize;
            let len = rd_u32(pack, 12 + index * 8) as usize;
            let audio = Audio::from_bytes(&pack[offset..offset + len]).expect("PA2 PSAU fixture");
            let bytes = audio.adpcm_bytes();
            assert!(
                next + bytes.len() as u32 <= 512 * 1024,
                "PA2 SPU bank overflow"
            );
            if map && index == 0 {
                self.target_addr = next;
                self.target_rate = audio.sample_rate_hz();
                self.target_len = bytes.len() as u32;
            }
            let address = SpuAddr::new(next);
            if safe {
                self.upload_timing
                    .merge(upload_adpcm_settled(address, bytes));
            } else {
                spu::upload_adpcm(address, bytes);
            }
            next = (next + bytes.len() as u32 + 7) & !7;
        }
        next
    }

    fn play_target(&self) {
        assert!(
            self.target_addr != 0 && self.target_rate != 0,
            "PA2 target not loaded"
        );
        let voice = Voice::new(DIALOGUE_VOICE);
        voice.configure_sample(
            SpuAddr::new(self.target_addr),
            self.target_rate,
            Volume::linear(1, 4),
            Adsr::sample(),
        );
        Voice::key_on(voice.mask());
    }

    fn capture_stage(&mut self, tick: u32) {
        let spucnt = unsafe { psx_io::read16(psx_io::spu::SPUCNT) };
        let spustat = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) };
        let read = |offset| unsafe { psx_io::read16(VOICE_BASE + offset) };
        let endx_lo = unsafe { psx_io::read16(ENDX_LO) };
        let endx_hi = unsafe { psx_io::read16(ENDX_HI) };
        self.records[self.stage as usize].fields = [
            ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF),
            ((spucnt as u32) << 16) | spustat as u32,
            ((read(0) as u32) << 16) | read(2) as u32,
            ((read(4) as u32) << 16) | read(6) as u32,
            ((read(8) as u32) << 16) | read(10) as u32,
            ((read(12) as u32) << 16) | read(14) as u32,
            ((endx_hi as u32) << 16) | endx_lo as u32,
            self.upload_timing.packed(),
            self.expected_tail_hash,
            self.observed_tail_hash,
        ];
        tty::print("hardware-tests: pa2 sample stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" endx=");
        tty::print(hex8(((endx_hi as u32) << 16) | endx_lo as u32).digits());
        tty::print(" tail=");
        tty::println(hex8(self.observed_tail_hash).digits());
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, 0);
        let crc = crc32(&out.bytes()[..BINARY_LEN - 4]);
        let crc_pos = BINARY_LEN - 4;
        binary[crc_pos..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        let payload_len = base64_encode(&binary, &mut payload);
        assert!(payload_len == BASE64_LEN, "PA2 Base64 layout drift");
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"PA2/");
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

    fn write_binary(&self, out: &mut BinaryBuffer<'_>, crc: u32) {
        out.push_bytes(b"PA2B");
        out.push_u8(1);
        out.push_u8(STAGE_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(PA2_LAYOUT_CRC);
        out.push_u32((self.target_rate << 16) | self.target_len);
        out.push_u32(self.target_hash);
        for record in self.records {
            for value in record.fields {
                out.push_u32(value);
            }
        }
        out.push_u32(crc);
        assert!(out.len() == BINARY_LEN, "PA2 binary layout drift");
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, self.binary_crc);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: pa2 PA2/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "HL VOICE BANK CONFORMANCE PA2", (255, 232, 128));
        font.draw_text(8, 20, "AUTOMATIC - RECORD COMPLETE RUN", (150, 170, 200));
        if !self.complete {
            let stage = self.stage as usize;
            font.draw_text(8, 42, "STAGE", (140, 160, 190));
            font.draw_text(64, 42, hex2(self.stage).as_str(), (232, 236, 244));
            font.draw_text(88, 42, stage_label(self.stage), stage_color(self.stage));
            font.draw_text(8, 58, stage_description(self.stage), (220, 224, 230));
            font.draw_text(8, 76, "TIME LEFT", (140, 160, 190));
            let remaining = STAGE_FRAMES[stage].saturating_sub(self.stage_frame);
            font.draw_text(88, 76, hex8(remaining as u32).digits(), (232, 236, 244));
            font.draw_text(8, 94, "BANKS", (140, 160, 190));
            font.draw_text(88, 94, hex2(PA2_CORE_COUNT).as_str(), (232, 236, 244));
            font.draw_text(112, 94, "+", (140, 160, 190));
            font.draw_text(128, 94, hex2(PA2_MAP_COUNT).as_str(), (232, 236, 244));
            font.draw_text(8, 124, "STAGE 0 BEEP PROVES OBS AUDIO", (112, 136, 170));
            font.draw_text(8, 140, "LATE LOUD TONE = END-BLOCK OVERRUN", (255, 128, 96));
            font.draw_text(8, 156, "DO NOT CHANGE VOLUME OR STOP OBS", (255, 216, 96));
            font.draw_text(8, 172, "QR APPEARS AFTER ALL 6 STAGES", (112, 136, 170));
            return;
        }

        font.draw_text(8, 36, "COMPLETE - HOLD CAMERA STEADY ON QR", (96, 240, 128));
        font.draw_text(8, 226, "X RERUN  DOWN PA1 CD AUDIO TEST", (150, 170, 200));
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

fn rd_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn put_u16(output: &mut [u8], offset: &mut usize, value: u16) {
    output[*offset..*offset + 2].copy_from_slice(&value.to_le_bytes());
    *offset += 2;
}

fn put_u32(output: &mut [u8], offset: &mut usize, value: u32) {
    output[*offset..*offset + 4].copy_from_slice(&value.to_le_bytes());
    *offset += 4;
}

fn build_pack(
    layout: &[(u32, u32, u32, bool)],
    guard_index: Option<usize>,
    output: &mut [u8],
) -> usize {
    let table_end = 8 + layout.len() * 8;
    let total = table_end
        + layout
            .iter()
            .map(|entry| 32 + entry.2 as usize * 16)
            .sum::<usize>();
    assert!(total <= output.len(), "PA2 staging buffer too small");
    output[..total].fill(0);
    output[..4].copy_from_slice(b"HSFX");
    output[4..8].copy_from_slice(&(layout.len() as u32).to_le_bytes());
    let mut data_offset = table_end;
    for (index, &(rate, samples, blocks, looped)) in layout.iter().enumerate() {
        let table = 8 + index * 8;
        let blob_len = 32 + blocks as usize * 16;
        output[table..table + 4].copy_from_slice(&(data_offset as u32).to_le_bytes());
        output[table + 4..table + 8].copy_from_slice(&(blob_len as u32).to_le_bytes());
        build_psau(
            rate,
            samples,
            blocks,
            looped,
            guard_index == Some(index),
            &mut output[data_offset..data_offset + blob_len],
        );
        data_offset += blob_len;
    }
    total
}

fn build_psau(rate: u32, samples: u32, blocks: u32, looped: bool, tone: bool, output: &mut [u8]) {
    let mut offset = 0usize;
    output[..4].copy_from_slice(b"PSAU");
    offset += 4;
    put_u16(output, &mut offset, 1);
    put_u16(output, &mut offset, 3);
    put_u32(output, &mut offset, 20 + blocks * 16);
    output[offset..offset + 4].copy_from_slice(&[1, 1, 0, 0]);
    offset += 4;
    put_u32(output, &mut offset, rate);
    put_u32(output, &mut offset, samples);
    put_u32(output, &mut offset, blocks);
    put_u32(output, &mut offset, u32::MAX);
    for block in 0..blocks {
        let is_last = block + 1 == blocks;
        let use_tone = tone || (blocks == PA2_MAP_LAYOUT[0].2 && block < 64);
        output[offset] = if use_tone { 0x00 } else { 0x0C };
        output[offset + 1] = if looped {
            if blocks == 1 {
                0x07
            } else if block == 0 {
                0x04
            } else if is_last {
                0x03
            } else {
                0
            }
        } else if is_last {
            0x01
        } else {
            0
        };
        output[offset + 2..offset + 16].fill(if use_tone { 0x78 } else { 0 });
        offset += 16;
    }
}

fn target_bytes(pack: &[u8]) -> Option<&[u8]> {
    let offset = rd_u32(pack, 8) as usize;
    let len = rd_u32(pack, 12) as usize;
    let audio = Audio::from_bytes(pack.get(offset..offset + len)?).ok()?;
    Some(audio.adpcm_bytes())
}

fn fnv16_bytes(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for pair in bytes.chunks_exact(2) {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        hash = (hash ^ value as u32).wrapping_mul(0x0100_0193);
    }
    hash
}

fn fnv32_words(words: &[u32]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &word in words {
        hash = (hash ^ (word & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (word >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Experimental comparison path.  It differs from the production SDK upload
/// only by waiting for SPUSTAT's delayed mode mirror before arming DMA and by
/// allowing the final FIFO words to drain before returning to Stop mode.
fn upload_adpcm_settled(dest: SpuAddr, bytes: &[u8]) -> UploadTiming {
    use psx_io::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    assert!(bytes.as_ptr() as usize % 4 == 0 && bytes.len().is_multiple_of(4));
    let words = (bytes.len() / 4) as u32;
    let block_size = if words % 16 == 0 {
        16
    } else if words % 8 == 0 {
        8
    } else if words % 4 == 0 {
        4
    } else if words % 2 == 0 {
        2
    } else {
        1
    };
    let block_count = words / block_size;
    let mut timing = UploadTiming::default();
    unsafe {
        let stopped = psx_io::read16(SPUCNT) & !0x0030;
        psx_io::write16(SPUCNT, stopped);
        timing.max_mode_polls = wait_mode(stopped);
        psx_io::write16(TRANSFER_CTRL, 0x0004);
        psx_io::write16(TRANSFER_ADDR, dest.byte_offset().wrapping_div(8) as u16);
        psx_io::write16(SPUCNT, stopped | 0x0020);
        timing.max_mode_polls = timing.max_mode_polls.max(wait_mode(stopped | 0x0020));

        dma::enable_channel(dma::Channel::Spu);
        dma::set_madr(dma::Channel::Spu, bytes.as_ptr() as u32);
        dma::set_bcr_block(dma::Channel::Spu, block_size as u16, block_count as u16);
        dma::set_chcr(
            dma::Channel::Spu,
            dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START,
        );
        let mut dma_guard = 0u32;
        while dma::is_busy(dma::Channel::Spu) && dma_guard < 1_000_000 {
            dma_guard += 1;
        }

        // Hardware exposes transfer-busy indications on bits 7 and 13 across
        // revisions.  A bounded poll plus a fixed FIFO-sized settling window
        // keeps this comparison safe even when one status bit is sticky.
        let mut drain = 0u16;
        while psx_io::read16(SPUSTAT) & 0x2080 != 0 && drain != u16::MAX {
            drain = drain.wrapping_add(1);
        }
        timing.max_drain_polls = drain;
        for _ in 0..4096 {
            core::hint::spin_loop();
        }
        psx_io::write16(SPUCNT, stopped);
        timing.max_mode_polls = timing.max_mode_polls.max(wait_mode(stopped));
    }
    timing
}

fn wait_mode(want: u16) -> u16 {
    let mut polls = 0u16;
    unsafe {
        while psx_io::read16(psx_io::spu::SPUSTAT) & 0x003F != want & 0x003F && polls != u16::MAX {
            polls = polls.wrapping_add(1);
        }
    }
    polls
}

fn stage_label(stage: u8) -> &'static str {
    match stage {
        0 => "IDLE + CALIBRATION",
        1 => "GAME BANK DMA UPLOAD",
        2 => "GAME VOICE ACTIVE",
        3 => "GAME VOICE END + GUARD",
        4 => "SETTLED DMA VOICE ACTIVE",
        _ => "SETTLED DMA END + GUARD",
    }
}

fn stage_description(stage: u8) -> &'static str {
    match stage {
        0 => "SPU VOICE PROVES OBS AUDIO CAPTURE",
        1 => "94 BACK-TO-BACK PRODUCTION UPLOADS",
        2 => "VOICE 15: SHORT MARKER THEN SILENCE",
        3 => "CORRECT END IS SILENT; OVERRUN IS LOUD",
        4 => "SAME BANK WITH MODE + FIFO SETTLING",
        _ => "COMPARE ENDX, TAIL HASH, AND OBS AUDIO",
    }
}

fn stage_color(stage: u8) -> (u8, u8, u8) {
    match stage {
        1 | 2 | 3 => (255, 128, 96),
        4 | 5 => (96, 240, 128),
        _ => (255, 216, 96),
    }
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
        self.bytes[self.len] = value;
        self.len += 1;
    }

    fn push_u32(&mut self, value: u32) {
        self.push_bytes(&value.to_le_bytes());
    }

    fn push_bytes(&mut self, values: &[u8]) {
        let end = self.len + values.len();
        self.bytes[self.len..end].copy_from_slice(values);
        self.len = end;
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
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
