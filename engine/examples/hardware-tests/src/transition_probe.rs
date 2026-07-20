//! PA3: exact Hazard Course full -> light -> t0a0 SPU-bank transition.
//!
//! The layouts and DMA call sequence match hl-psx, but every ADPCM payload is
//! synthetic. An intentionally unsafe live map-bank overwrite is the positive
//! control; a stopped-voice overwrite is the production-safe comparison.

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, spu_dma_read};

include!(concat!(env!("OUT_DIR"), "/pa3_meta.rs"));

#[repr(C, align(4))]
struct SampleStaging([u32; 16_400]); // 65,600 bytes; largest source sample is 65,423 B.

static mut SAMPLE_STAGING: SampleStaging = SampleStaging([0; 16_400]);

const STAGE_COUNT: usize = 6;
const FIELD_COUNT: usize = 10;
const STAGE_FRAMES: [u16; STAGE_COUNT] = [180, 180, 300, 300, 300, 360];
const CAPTURE_FRAMES: [u16; STAGE_COUNT] = [120, 120, 180, 180, 180, 240];
const SPU_SAMPLE_BASE: u32 = 0x1010;
const CALIBRATION_ADDR: u32 = 0x7FF00;
const MENU_VOICE: u8 = 16;
const DIALOGUE_VOICE: u8 = 15;
const LOOP_VOICE: u8 = 17;
const MAP_ONESHOT_VOICE: u8 = 0;
const MAP_ACTIVE_MASK: u32 = (1 << MAP_ONESHOT_VOICE) | (1 << DIALOGUE_VOICE) | (1 << LOOP_VOICE);
const READBACK_BYTES: usize = 64;

const CALIBRATION_TONE: [u8; 16] = [
    0x00, 0x07, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
];

const QR_VERSION: Version = Version::new(15);
const QR_SIZE: usize = 77;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 272;
const BASE64_LEN: usize = 364;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

const ENDX_LO: u32 = psx_io::spu::SPU_BASE + 0x19C;
const ENDX_HI: u32 = psx_io::spu::SPU_BASE + 0x19E;
const SPU_DELAY: u32 = 0x1F80_1014;

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

#[derive(Copy, Clone, PartialEq, Eq)]
enum Pattern {
    Silence,
    FullMenu,
    MapActive,
    MapOverwrite,
}

pub(crate) struct TransitionProbe {
    stage: u8,
    stage_frame: u16,
    complete: bool,
    run: u8,
    records: [StageRecord; STAGE_COUNT],
    full_menu_addr: u32,
    map_base: u32,
    map_addrs: [u32; 4],
    event_vblanks: [u16; STAGE_COUNT],
    expected_hash: u32,
    observed_hash: u32,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl TransitionProbe {
    pub(crate) const fn new() -> Self {
        Self {
            stage: 0,
            stage_frame: 0,
            complete: false,
            run: 0,
            records: [StageRecord::empty(); STAGE_COUNT],
            full_menu_addr: 0,
            map_base: 0,
            map_addrs: [0; 4],
            event_vblanks: [0; STAGE_COUNT],
            expected_hash: 0,
            observed_hash: 0,
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
        self.full_menu_addr = 0;
        self.map_base = 0;
        self.map_addrs = [0; 4];
        self.event_vblanks = [0; STAGE_COUNT];
        self.expected_hash = 0;
        self.observed_hash = 0;
        self.qr_size = 0;
        spu::init();
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::println("hardware-tests: pa3 begin Hazard Course transition probe");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    /// Returns true after an intentional blocking SPU transfer. The scene must
    /// discard accumulated fixed-update debt so stage frames remain wall-clock
    /// frames instead of catching up in a burst.
    pub(crate) fn update(&mut self, tick: u32) -> bool {
        if self.complete {
            return false;
        }

        let mut realign = false;
        match (self.stage, self.stage_frame) {
            (0, 15) => Voice::key_on(Voice::V0.mask()),
            (0, 45) => {
                Voice::key_off(Voice::V0.mask());
                Voice::V0.set_volume(Volume::SILENCE, Volume::SILENCE);
            }
            (2, 15) => {
                self.play_voice(MENU_VOICE, self.full_menu_addr, PA3_FULL_LAYOUT[16].0, 1, 8)
            }
            (2, 45) => {
                self.timed_event(|this| this.load_light_and_map(Pattern::MapActive));
                realign = true;
            }
            (5, 45) => {
                self.timed_event(|this| {
                    this.stop_map_voices();
                    this.upload_map(Pattern::MapOverwrite);
                    this.play_voice(0, this.map_addrs[3], PA3_MAP_LAYOUT[24].0, 1, 16);
                });
                realign = true;
            }
            _ => {}
        }

        let stage = self.stage as usize;
        if self.stage_frame == CAPTURE_FRAMES[stage] {
            self.capture_stage(tick);
        }

        self.stage_frame = self.stage_frame.saturating_add(1);
        if self.stage_frame < STAGE_FRAMES[stage] {
            return realign;
        }

        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            if self.apply_stage() {
                realign = true;
            }
        } else {
            self.stop_map_voices();
            let menu = Voice::new(MENU_VOICE);
            menu.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(menu.mask());
            self.complete = true;
            self.encode_qr();
            self.print_payload();
        }
        realign
    }

    fn apply_stage(&mut self) -> bool {
        let blocking = match self.stage {
            0 => {
                spu::upload_adpcm(SpuAddr::new(CALIBRATION_ADDR), &CALIBRATION_TONE);
                Voice::V0.set_volume(Volume::linear(1, 8), Volume::linear(1, 8));
                Voice::V0.set_pitch(Pitch::UNITY);
                Voice::V0.set_start_addr(SpuAddr::new(CALIBRATION_ADDR));
                Voice::V0.set_adsr(Adsr::sample());
                false
            }
            1 => {
                self.timed_event(|this| this.load_full());
                true
            }
            2 => false,
            3 => {
                self.play_map_voices();
                false
            }
            4 => {
                // Deliberately wrong: rewrite a bank while three voices still
                // read it. This must sound bad, proving the recording can see
                // the class of failure we are trying to discriminate.
                self.timed_event(|this| this.upload_map(Pattern::MapOverwrite));
                true
            }
            5 => {
                // Recreate the same active-voice premise, then perform the safe
                // handoff at frame 45 after all known map-backed voices stop.
                self.timed_event(|this| {
                    this.stop_map_voices();
                    this.upload_map(Pattern::MapActive);
                    this.play_map_voices();
                });
                true
            }
            _ => unreachable!(),
        };
        tty::print("hardware-tests: pa3 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" label=");
        tty::println(stage_label(self.stage));
        blocking
    }

    fn timed_event(&mut self, event: impl FnOnce(&mut Self)) {
        let before = interrupts::vblank_count();
        event(self);
        let elapsed = interrupts::vblank_count()
            .wrapping_sub(before)
            .min(u16::MAX as u32) as u16;
        self.event_vblanks[self.stage as usize] =
            self.event_vblanks[self.stage as usize].saturating_add(elapsed);
    }

    fn load_full(&mut self) {
        spu::init();
        spu::enable_cd_audio(false);
        let end = self.upload_layout(PA3_FULL_LAYOUT, Pattern::FullMenu, SPU_SAMPLE_BASE, true);
        assert_eq!(end - SPU_SAMPLE_BASE, PA3_FULL_BYTES, "PA3 full bank size");
    }

    fn load_light_and_map(&mut self, map_pattern: Pattern) {
        // This is the game transition exactly: the active menu bank is stopped
        // by init, chunk 3050 replaces chunk 3000, and t0a0 follows it.
        spu::init();
        spu::enable_cd_audio(false);
        let map_base =
            self.upload_layout(PA3_LIGHT_LAYOUT, Pattern::Silence, SPU_SAMPLE_BASE, false);
        assert_eq!(
            map_base - SPU_SAMPLE_BASE,
            PA3_LIGHT_BYTES,
            "PA3 light bank size"
        );
        self.map_base = map_base;
        let end = self.upload_layout(PA3_MAP_LAYOUT, map_pattern, map_base, true);
        assert_eq!(end - map_base, PA3_MAP_BYTES, "PA3 map bank size");
        assert!(end <= 512 * 1024, "PA3 fixture exceeds SPU RAM");
    }

    fn upload_map(&mut self, pattern: Pattern) {
        assert!(self.map_base != 0, "PA3 map base not established");
        let end = self.upload_layout(PA3_MAP_LAYOUT, pattern, self.map_base, true);
        assert_eq!(end - self.map_base, PA3_MAP_BYTES, "PA3 replacement size");
    }

    fn upload_layout(
        &mut self,
        layout: &[(u32, u32, u32, bool)],
        pattern: Pattern,
        mut next: u32,
        capture_readback: bool,
    ) -> u32 {
        let mut readback_addr = 0u32;
        let mut expected = 0u32;
        for (index, &(_, _, blocks, looped)) in layout.iter().enumerate() {
            let len = blocks as usize * 16;
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(SAMPLE_STAGING.0.as_mut_ptr() as *mut u8, len)
            };
            build_adpcm(bytes, blocks, looped, sample_pattern(pattern, index));

            if pattern == Pattern::FullMenu && index == 16 {
                self.full_menu_addr = next;
            }
            if matches!(pattern, Pattern::MapActive | Pattern::MapOverwrite) {
                match index {
                    0 => self.map_addrs[0] = next,
                    2 => self.map_addrs[1] = next,
                    13 => self.map_addrs[2] = next,
                    24 => self.map_addrs[3] = next,
                    _ => {}
                }
            }
            if capture_readback && index == 0 {
                readback_addr = next;
                expected = fnv16_bytes(&bytes[..READBACK_BYTES]);
            }
            spu::upload_adpcm(SpuAddr::new(next), bytes);
            next += len as u32;
        }
        if capture_readback {
            self.expected_hash = expected;
            self.observed_hash = stable_read_hash(readback_addr);
        }
        next
    }

    fn play_map_voices(&self) {
        self.play_voice(
            MAP_ONESHOT_VOICE,
            self.map_addrs[1],
            PA3_MAP_LAYOUT[2].0,
            1,
            24,
        );
        self.play_voice(
            DIALOGUE_VOICE,
            self.map_addrs[0],
            PA3_MAP_LAYOUT[0].0,
            1,
            16,
        );
        self.play_voice(LOOP_VOICE, self.map_addrs[2], PA3_MAP_LAYOUT[13].0, 1, 24);
    }

    fn play_voice(&self, index: u8, addr: u32, rate: u32, num: u16, den: u16) {
        assert!(addr != 0, "PA3 voice address not loaded");
        let voice = Voice::new(index);
        voice.configure_sample(
            SpuAddr::new(addr),
            rate,
            Volume::linear(num, den),
            Adsr::sample(),
        );
        Voice::key_on(voice.mask());
    }

    fn stop_map_voices(&self) {
        for index in [MAP_ONESHOT_VOICE, DIALOGUE_VOICE, LOOP_VOICE] {
            Voice::new(index).set_volume(Volume::SILENCE, Volume::SILENCE);
        }
        Voice::key_off(MAP_ACTIVE_MASK);
    }

    fn capture_stage(&mut self, tick: u32) {
        let spucnt = unsafe { psx_io::read16(psx_io::spu::SPUCNT) };
        let spustat = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) };
        let endx_lo = unsafe { psx_io::read16(ENDX_LO) };
        let endx_hi = unsafe { psx_io::read16(ENDX_HI) };
        let voice_state = |voice: u8| {
            let base = psx_io::spu::SPU_BASE + voice as u32 * 16;
            let volume = unsafe { psx_io::read16(base) };
            let current = unsafe { psx_io::read16(base + 12) };
            ((volume as u32) << 16) | current as u32
        };
        self.records[self.stage as usize].fields = [
            ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF),
            ((spucnt as u32) << 16) | spustat as u32,
            ((endx_hi as u32) << 16) | endx_lo as u32,
            voice_state(MAP_ONESHOT_VOICE),
            voice_state(DIALOGUE_VOICE),
            voice_state(MENU_VOICE),
            voice_state(LOOP_VOICE),
            self.event_vblanks[self.stage as usize] as u32,
            self.expected_hash,
            self.observed_hash,
        ];
        tty::print("hardware-tests: pa3 sample stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" endx=");
        tty::print(hex8(((endx_hi as u32) << 16) | endx_lo as u32).digits());
        tty::print(" vblanks=");
        tty::print(hex8(self.event_vblanks[self.stage as usize] as u32).digits());
        tty::print(" readback=");
        tty::println(hex8(self.observed_hash).digits());
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, 0);
        let crc = crc32(&out.bytes()[..BINARY_LEN - 4]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        assert_eq!(
            base64_encode(&binary, &mut payload),
            BASE64_LEN,
            "PA3 Base64 layout"
        );
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"PA3/");
        append(&mut text, &mut text_len, &payload);
        append(&mut text, &mut text_len, b"/C:");
        append(&mut text, &mut text_len, hex8(crc).digits().as_bytes());
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
        out.push_bytes(b"PA3B");
        out.push_u8(1);
        out.push_u8(STAGE_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(PA3_LAYOUT_CRC);
        out.push_u32(PA3_FULL_BYTES);
        out.push_u32(PA3_LIGHT_BYTES);
        out.push_u32(PA3_MAP_BYTES);
        out.push_u32(READBACK_BYTES as u32);
        for record in self.records {
            for value in record.fields {
                out.push_u32(value);
            }
        }
        out.push_u32(crc);
        assert_eq!(out.len(), BINARY_LEN, "PA3 binary layout drift");
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, self.binary_crc);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: pa3 PA3/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "HL BANK TRANSITION PA3", (255, 232, 128));
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
            font.draw_text(8, 104, "STAGE 0 BEEP PROVES OBS AUDIO", (112, 136, 170));
            font.draw_text(8, 120, "STAGE 4 MUST SOUND BAD: CONTROL", (255, 128, 96));
            font.draw_text(8, 136, "STAGE 5 MUST GO QUIET AFTER SWITCH", (96, 240, 128));
            font.draw_text(8, 152, "DO NOT CHANGE VOLUME OR STOP OBS", (255, 216, 96));
            font.draw_text(8, 168, "QR APPEARS AFTER ALL 6 STAGES", (112, 136, 170));
            return;
        }

        font.draw_text(8, 36, "COMPLETE - HOLD CAMERA STEADY ON QR", (96, 240, 128));
        font.draw_text(8, 226, "X RERUN  DOWN PA2 VOICE TEST", (150, 170, 200));
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

#[derive(Copy, Clone)]
enum SamplePattern {
    Silent,
    First64Tone,
    Tone,
    OverwriteTone,
}

fn sample_pattern(pattern: Pattern, index: usize) -> SamplePattern {
    match pattern {
        Pattern::FullMenu if index == 16 => SamplePattern::Tone,
        Pattern::MapActive if matches!(index, 0 | 2 | 13) => SamplePattern::First64Tone,
        Pattern::MapOverwrite if matches!(index, 0 | 2 | 13) => SamplePattern::OverwriteTone,
        Pattern::MapOverwrite if index == 24 => SamplePattern::First64Tone,
        _ => SamplePattern::Silent,
    }
}

fn build_adpcm(output: &mut [u8], blocks: u32, looped: bool, pattern: SamplePattern) {
    assert_eq!(output.len(), blocks as usize * 16);
    for block in 0..blocks {
        let offset = block as usize * 16;
        let is_last = block + 1 == blocks;
        let tone_byte = match pattern {
            SamplePattern::Silent => 0,
            SamplePattern::First64Tone if block < 64 => 0x78,
            SamplePattern::First64Tone => 0,
            SamplePattern::Tone => 0x78,
            SamplePattern::OverwriteTone => 0x45,
        };
        output[offset] = if tone_byte != 0 { 0x00 } else { 0x0C };
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
        output[offset + 2..offset + 16].fill(tone_byte);
    }
}

fn stable_read_hash(addr: u32) -> u32 {
    let original_delay = unsafe { psx_io::read32(SPU_DELAY) };
    unsafe { psx_io::write32(SPU_DELAY, original_delay | 0x0200_0000) };
    for _ in 0..64 {
        core::hint::spin_loop();
    }
    let mut back = [0u32; READBACK_BYTES / 4];
    spu_dma_read(addr, &mut back);
    unsafe { psx_io::write32(SPU_DELAY, original_delay) };
    fnv32_words(&back)
}

fn fnv16_bytes(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for pair in bytes.chunks_exact(2) {
        hash = (hash ^ u16::from_le_bytes([pair[0], pair[1]]) as u32).wrapping_mul(0x0100_0193);
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

fn stage_label(stage: u8) -> &'static str {
    match stage {
        0 => "IDLE + CALIBRATION",
        1 => "FULL MENU BANK",
        2 => "FULL -> LIGHT + T0A0",
        3 => "MAP VOICES ACTIVE",
        4 => "UNSAFE LIVE OVERWRITE",
        _ => "SAFE STOP + OVERWRITE",
    }
}

fn stage_description(stage: u8) -> &'static str {
    match stage {
        0 => "SHORT BEEP CALIBRATES OBS CAPTURE",
        1 => "EXACT CHUNK 3000 DIMENSIONS + 68 DMAS",
        2 => "ACTIVE MENU V16; INIT; 3050 + 3198",
        3 => "V0 + V15 + LOOP V17 READ MAP BANK",
        4 => "POSITIVE CONTROL: REWRITE WHILE ACTIVE",
        _ => "STOP ALL MAP OWNERS BEFORE REWRITE",
    }
}

fn stage_color(stage: u8) -> (u8, u8, u8) {
    match stage {
        4 => (255, 96, 96),
        5 => (96, 240, 128),
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
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize]
        } else {
            b'='
        };
        output[target + 3] = if remaining > 2 {
            ALPHABET[(c & 63) as usize]
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
