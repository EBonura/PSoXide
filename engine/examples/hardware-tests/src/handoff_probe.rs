//! PA4: selectable isolation of the stale-menu-voice bank-transition fault.
//!
//! PA3 proved that a naturally-ended voice followed by `spu::init()` and the
//! full -> light -> t0a0 replacement produces persistent, data-dependent noise
//! on silicon but not in PSoXide. PA4 keeps the exact timing while varying only
//! explicit voice shutdown and its VBlank delay. A fifth split variant places
//! observation windows between init, light upload, map upload, and readback.

use psx_engine::{button, Ctx};
use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, spu_dma_read};

include!(concat!(env!("OUT_DIR"), "/pa3_meta.rs"));

#[repr(C, align(4))]
struct SampleStaging([u32; 16_400]);

static mut SAMPLE_STAGING: SampleStaging = SampleStaging([0; 16_400]);

const STAGE_COUNT: usize = 7;
const FIELD_COUNT: usize = 10;
const STAGE_FRAMES: [u16; STAGE_COUNT] = [180, 180, 45, 180, 180, 180, 180];
const CAPTURE_FRAMES: [u16; STAGE_COUNT] = [120, 120, 44, 120, 120, 120, 120];
const SELECTION_FRAMES: u16 = 300;
const SPU_SAMPLE_BASE: u32 = 0x1010;
const CALIBRATION_ADDR: u32 = 0x7FF00;
const MENU_VOICE: u8 = 16;
const READBACK_BYTES: usize = 64;
const SPU_DELAY: u32 = 0x1F80_1014;

const CALIBRATION_TONE: [u8; 16] = [
    0x00, 0x07, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
];

const QR_VERSION: Version = Version::new(17);
const QR_SIZE: usize = 85;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 320;
const BASE64_LEN: usize = 428;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

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

#[derive(Copy, Clone, PartialEq, Eq)]
enum Variant {
    Baseline,
    Safe0,
    Safe1,
    Safe2,
    Split,
}

impl Variant {
    const COUNT: u8 = 5;

    const fn code(self) -> u8 {
        match self {
            Self::Baseline => 0,
            Self::Safe0 => 1,
            Self::Safe1 => 2,
            Self::Safe2 => 3,
            Self::Split => 4,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code % Self::COUNT {
            0 => Self::Baseline,
            1 => Self::Safe0,
            2 => Self::Safe1,
            3 => Self::Safe2,
            _ => Self::Split,
        }
    }

    const fn wait_vblanks(self) -> u8 {
        match self {
            Self::Safe1 => 1,
            Self::Safe2 => 2,
            _ => 0,
        }
    }

    const fn explicit_stop(self) -> bool {
        matches!(self, Self::Safe0 | Self::Safe1 | Self::Safe2)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Baseline => "BASELINE EXACT (NO PRE-STOP)",
            Self::Safe0 => "SAFE PRE-STOP + WAIT 0",
            Self::Safe1 => "SAFE PRE-STOP + WAIT 1",
            Self::Safe2 => "SAFE PRE-STOP + WAIT 2",
            Self::Split => "SPLIT UNSAFE DIAGNOSTIC",
        }
    }

    const fn short(self) -> &'static str {
        match self {
            Self::Baseline => "BASE",
            Self::Safe0 => "SAFE0",
            Self::Safe1 => "SAFE1",
            Self::Safe2 => "SAFE2",
            Self::Split => "SPLIT",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Pattern {
    Silent,
    FullMenu,
    MapFixture,
}

pub(crate) struct HandoffProbe {
    selecting: bool,
    selection_frame: u16,
    variant: Variant,
    stage: u8,
    stage_frame: u16,
    complete: bool,
    run: u8,
    records: [StageRecord; STAGE_COUNT],
    full_menu_addr: u32,
    map_base: u32,
    event_vblank_before: [u32; STAGE_COUNT],
    event_vblank_after: [u32; STAGE_COUNT],
    expected_hash: u32,
    observed_hash: u32,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl HandoffProbe {
    pub(crate) const fn new() -> Self {
        Self {
            selecting: true,
            selection_frame: 0,
            variant: Variant::Safe2,
            stage: 0,
            stage_frame: 0,
            complete: false,
            run: 0,
            records: [StageRecord::empty(); STAGE_COUNT],
            full_menu_addr: 0,
            map_base: 0,
            event_vblank_before: [0; STAGE_COUNT],
            event_vblank_after: [0; STAGE_COUNT],
            expected_hash: 0,
            observed_hash: 0,
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn start(&mut self) {
        self.selecting = true;
        self.selection_frame = 0;
        self.variant = Variant::Safe2;
        self.complete = false;
        self.qr_size = 0;
        spu::init();
        spu::enable_cd_audio(false);
        tty::println("hardware-tests: pa4 selection; default SAFE2; LEFT/RIGHT then X");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    /// Returns `(timing_realign, consume_navigation_input)`.
    pub(crate) fn update(&mut self, ctx: &mut Ctx) -> (bool, bool) {
        if self.complete {
            return (false, false);
        }

        if self.selecting {
            if ctx.just_pressed(button::LEFT) {
                self.variant =
                    Variant::from_code(self.variant.code().wrapping_add(Variant::COUNT - 1));
                self.selection_frame = 0;
            } else if ctx.just_pressed(button::RIGHT) {
                self.variant = Variant::from_code(self.variant.code().wrapping_add(1));
                self.selection_frame = 0;
            }
            let start = ctx.just_pressed(button::CROSS) || self.selection_frame >= SELECTION_FRAMES;
            self.selection_frame = self.selection_frame.saturating_add(1);
            if start {
                self.begin_run();
            }
            return (false, true);
        }

        let tick = ctx.sim_tick.as_u32();
        if self.stage == 0 {
            if self.stage_frame == 15 {
                Voice::key_on(Voice::V0.mask());
            } else if self.stage_frame == 45 {
                Voice::key_off(Voice::V0.mask());
                Voice::V0.set_volume(Volume::SILENCE, Volume::SILENCE);
            }
        } else if self.stage == 2 && self.stage_frame == 15 {
            self.play_menu();
        }

        let stage = self.stage as usize;
        if self.stage_frame == CAPTURE_FRAMES[stage] {
            self.capture_stage(tick);
        }

        self.stage_frame = self.stage_frame.saturating_add(1);
        if self.stage_frame < STAGE_FRAMES[stage] {
            return (false, true);
        }

        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            let realign = self.apply_stage();
            (realign, true)
        } else {
            self.complete = true;
            self.encode_qr();
            self.print_payload();
            (false, false)
        }
    }

    fn begin_run(&mut self) {
        self.selecting = false;
        self.stage = 0;
        self.stage_frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.records = [StageRecord::empty(); STAGE_COUNT];
        self.full_menu_addr = 0;
        self.map_base = 0;
        self.event_vblank_before = [0; STAGE_COUNT];
        self.event_vblank_after = [0; STAGE_COUNT];
        self.expected_hash = 0;
        self.observed_hash = 0;
        self.qr_size = 0;
        spu::init();
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::print("hardware-tests: pa4 begin variant=");
        tty::println(self.variant.short());
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
                self.timed_event(|this| this.apply_transition());
                true
            }
            4 if self.variant == Variant::Split => {
                self.timed_event(|this| this.upload_light());
                true
            }
            5 if self.variant == Variant::Split => {
                self.timed_event(|this| this.upload_map());
                true
            }
            6 => {
                self.timed_event(|this| this.readback_map());
                true
            }
            _ => false,
        };
        tty::print("hardware-tests: pa4 stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" variant=");
        tty::print(self.variant.short());
        tty::print(" label=");
        tty::println(self.stage_label());
        blocking
    }

    fn apply_transition(&mut self) {
        if self.variant.explicit_stop() {
            let menu = Voice::new(MENU_VOICE);
            menu.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(menu.mask());
            for _ in 0..self.variant.wait_vblanks() {
                interrupts::wait_vblank();
            }
        }

        spu::init();
        spu::enable_cd_audio(false);
        if self.variant != Variant::Split {
            self.upload_light();
            self.upload_map();
        }
    }

    fn timed_event(&mut self, event: impl FnOnce(&mut Self)) {
        let stage = self.stage as usize;
        let before = interrupts::vblank_count();
        event(self);
        let after = interrupts::vblank_count();
        self.event_vblank_before[stage] = before;
        self.event_vblank_after[stage] = after;
    }

    fn load_full(&mut self) {
        spu::init();
        spu::enable_cd_audio(false);
        let end = self.upload_layout(PA3_FULL_LAYOUT, Pattern::FullMenu, SPU_SAMPLE_BASE);
        assert_eq!(end - SPU_SAMPLE_BASE, PA3_FULL_BYTES, "PA4 full bank size");
    }

    fn upload_light(&mut self) {
        let end = self.upload_layout(PA3_LIGHT_LAYOUT, Pattern::Silent, SPU_SAMPLE_BASE);
        assert_eq!(
            end - SPU_SAMPLE_BASE,
            PA3_LIGHT_BYTES,
            "PA4 light bank size"
        );
        self.map_base = end;
    }

    fn upload_map(&mut self) {
        assert!(self.map_base != 0, "PA4 map base not established");
        let end = self.upload_layout(PA3_MAP_LAYOUT, Pattern::MapFixture, self.map_base);
        assert_eq!(end - self.map_base, PA3_MAP_BYTES, "PA4 map bank size");
        assert!(end <= 512 * 1024, "PA4 fixture exceeds SPU RAM");
    }

    fn upload_layout(
        &mut self,
        layout: &[(u32, u32, u32, bool)],
        pattern: Pattern,
        mut next: u32,
    ) -> u32 {
        for (index, &(_, _, blocks, looped)) in layout.iter().enumerate() {
            let len = blocks as usize * 16;
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(SAMPLE_STAGING.0.as_mut_ptr() as *mut u8, len)
            };
            build_adpcm(bytes, blocks, looped, sample_pattern(pattern, index));
            if pattern == Pattern::FullMenu && index == 16 {
                self.full_menu_addr = next;
            }
            if pattern == Pattern::MapFixture && index == 0 {
                self.expected_hash = fnv16_bytes(&bytes[..READBACK_BYTES]);
            }
            spu::upload_adpcm(SpuAddr::new(next), bytes);
            next += len as u32;
        }
        next
    }

    fn play_menu(&self) {
        assert!(self.full_menu_addr != 0, "PA4 menu sample not loaded");
        let voice = Voice::new(MENU_VOICE);
        voice.configure_sample(
            SpuAddr::new(self.full_menu_addr),
            PA3_FULL_LAYOUT[16].0,
            Volume::linear(1, 8),
            Adsr::sample(),
        );
        Voice::key_on(voice.mask());
    }

    fn readback_map(&mut self) {
        assert!(self.map_base != 0, "PA4 map bank not loaded");
        self.observed_hash = stable_read_hash(self.map_base);
    }

    fn capture_stage(&mut self, tick: u32) {
        let stage = self.stage as usize;
        let spucnt = unsafe { psx_io::read16(psx_io::spu::SPUCNT) };
        let spustat = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) };
        let endx_lo = unsafe { psx_io::read16(ENDX_LO) };
        let endx_hi = unsafe { psx_io::read16(ENDX_HI) };
        let base = psx_io::spu::SPU_BASE + MENU_VOICE as u32 * 16;
        let read = |offset| unsafe { psx_io::read16(base + offset) };
        let event_vblanks = self.event_vblank_after[stage]
            .wrapping_sub(self.event_vblank_before[stage])
            .min(u16::MAX as u32);
        // PA4 schema v2 uses the final two fields for raw VBlank clock samples
        // on transition stages. The readback stage keeps the two SPU RAM hashes.
        // This makes an impossible elapsed value distinguishable from a damaged
        // counter or subtraction bug without increasing the QR payload.
        let (detail_a, detail_b) = if stage + 1 == STAGE_COUNT {
            (self.expected_hash, self.observed_hash)
        } else {
            (
                self.event_vblank_before[stage],
                self.event_vblank_after[stage],
            )
        };
        self.records[stage].fields = [
            ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF),
            ((spucnt as u32) << 16) | spustat as u32,
            ((endx_hi as u32) << 16) | endx_lo as u32,
            ((read(0) as u32) << 16) | read(12) as u32,
            ((read(4) as u32) << 16) | read(6) as u32,
            ((read(8) as u32) << 16) | read(14) as u32,
            nonzero_voice_volume_mask(),
            event_vblanks,
            detail_a,
            detail_b,
        ];
        tty::print("hardware-tests: pa4 sample stage=");
        tty::print(hex2(self.stage).as_str());
        tty::print(" endx=");
        tty::print(hex8(((endx_hi as u32) << 16) | endx_lo as u32).digits());
        tty::print(" voices=");
        tty::print(hex8(nonzero_voice_volume_mask()).digits());
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
            "PA4 Base64 layout"
        );
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"PA4/");
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
        out.push_bytes(b"PA4B");
        out.push_u8(2);
        out.push_u8(STAGE_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(PA3_LAYOUT_CRC);
        out.push_u32(self.variant.code() as u32);
        out.push_u32(self.variant.wait_vblanks() as u32);
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
        assert_eq!(out.len(), BINARY_LEN, "PA4 binary layout drift");
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, self.binary_crc);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: pa4 PA4/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "HL VOICE HANDOFF PA4", (255, 232, 128));
        if self.selecting {
            font.draw_text(
                8,
                24,
                "SELECT RUN - REBOOT BETWEEN VARIANTS",
                (150, 170, 200),
            );
            font.draw_text(8, 54, "VARIANT", (140, 160, 190));
            font.draw_text(72, 54, self.variant.label(), (255, 216, 96));
            font.draw_text(8, 82, "LEFT/RIGHT CHOOSE   X START", (232, 236, 244));
            font.draw_text(8, 102, "RUN SAFE2 FIRST", (96, 240, 128));
            font.draw_text(8, 118, "THEN REBOOT AND RUN SAFE0 / SAFE1", (150, 170, 200));
            font.draw_text(
                8,
                134,
                "BASE REPRODUCES PA3; SPLIT ISOLATES",
                (255, 128, 96),
            );
            let remaining = SELECTION_FRAMES.saturating_sub(self.selection_frame);
            font.draw_text(8, 168, "AUTO SAFE2", (140, 160, 190));
            font.draw_text(112, 168, hex8(remaining as u32).digits(), (232, 236, 244));
            return;
        }

        font.draw_text(8, 20, self.variant.label(), (150, 170, 200));
        if !self.complete {
            let stage = self.stage as usize;
            font.draw_text(8, 42, "STAGE", (140, 160, 190));
            font.draw_text(64, 42, hex2(self.stage).as_str(), (232, 236, 244));
            font.draw_text(88, 42, self.stage_label(), self.stage_color());
            font.draw_text(8, 58, self.stage_description(), (220, 224, 230));
            font.draw_text(8, 76, "TIME LEFT", (140, 160, 190));
            let remaining = STAGE_FRAMES[stage].saturating_sub(self.stage_frame);
            font.draw_text(88, 76, hex8(remaining as u32).digits(), (232, 236, 244));
            font.draw_text(8, 108, "NOTE EXACT NOISE ONSET STAGE", (112, 136, 170));
            font.draw_text(8, 124, "DO NOT CHANGE VOLUME OR STOP OBS", (255, 216, 96));
            font.draw_text(8, 140, "QR APPEARS AFTER READBACK STAGE", (112, 136, 170));
            return;
        }

        font.draw_text(8, 36, "COMPLETE - HOLD CAMERA STEADY ON QR", (96, 240, 128));
        font.draw_text(8, 230, "X SELECT AGAIN  DOWN PA3", (150, 170, 200));
        if self.qr_size as usize != QR_SIZE {
            font.draw_text(88, 112, "QR ENCODE FAILED", (255, 96, 96));
            return;
        }
        let total = (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE;
        let left = (320 - total) / 2;
        let top = 42;
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

    fn stage_label(&self) -> &'static str {
        match self.stage {
            0 => "IDLE + CALIBRATION",
            1 => "FULL MENU BANK",
            2 => "V16 NATURAL END",
            3 if self.variant == Variant::Split => "SPU INIT ONLY",
            3 => "PRE-STOP + EXACT TRANSITION",
            4 if self.variant == Variant::Split => "LIGHT BANK ONLY",
            4 => "POST-TRANSITION HOLD A",
            5 if self.variant == Variant::Split => "T0A0 MAP BANK ONLY",
            5 => "POST-TRANSITION HOLD B",
            _ => "SPU READBACK ONLY",
        }
    }

    fn stage_description(&self) -> &'static str {
        match self.stage {
            0 => "SHORT BEEP CALIBRATES OBS CAPTURE",
            1 => "EXACT CHUNK 3000 + 68 PRODUCTION DMAS",
            2 => "V16 ENDS; VOLUME REGISTER REMAINS SET",
            3 if self.variant == Variant::Baseline => "PA3 ORDER: INIT; 3050; 3198",
            3 if self.variant == Variant::Split => "NO PRE-STOP; INIT; THEN OBSERVE",
            3 => "ZERO V16; KEYOFF; WAIT; INIT; 3050; 3198",
            4 if self.variant == Variant::Split => "UPLOAD 3050; THEN OBSERVE",
            4 => "NO ADDITIONAL SPU OPERATIONS",
            5 if self.variant == Variant::Split => "UPLOAD 3198; THEN OBSERVE",
            5 => "NO ADDITIONAL SPU OPERATIONS",
            _ => "64-BYTE STABLE READ; HASH IN QR",
        }
    }

    fn stage_color(&self) -> (u8, u8, u8) {
        if self.variant.explicit_stop() && self.stage == 3 {
            (96, 240, 128)
        } else if self.variant == Variant::Baseline && self.stage == 3 {
            (255, 96, 96)
        } else {
            (255, 216, 96)
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
}

fn sample_pattern(pattern: Pattern, index: usize) -> SamplePattern {
    match pattern {
        Pattern::FullMenu if index == 16 => SamplePattern::Tone,
        Pattern::MapFixture if matches!(index, 0 | 2 | 13) => SamplePattern::First64Tone,
        _ => SamplePattern::Silent,
    }
}

fn build_adpcm(output: &mut [u8], blocks: u32, looped: bool, pattern: SamplePattern) {
    assert_eq!(output.len(), blocks as usize * 16);
    for block in 0..blocks {
        let offset = block as usize * 16;
        let is_last = block + 1 == blocks;
        let tone = match pattern {
            SamplePattern::Silent => false,
            SamplePattern::First64Tone => block < 64,
            SamplePattern::Tone => true,
        };
        output[offset] = if tone { 0x00 } else { 0x0C };
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
        output[offset + 2..offset + 16].fill(if tone { 0x78 } else { 0 });
    }
}

fn nonzero_voice_volume_mask() -> u32 {
    let mut mask = 0u32;
    for voice in 0..24u32 {
        let base = psx_io::spu::SPU_BASE + voice * 16;
        let left = unsafe { psx_io::read16(base) };
        let right = unsafe { psx_io::read16(base + 2) };
        if left != 0 || right != 0 {
            mask |= 1 << voice;
        }
    }
    mask
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
