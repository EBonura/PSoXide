//! PA4: selectable isolation of the stale-menu-voice bank-transition fault.
//!
//! PA3 proved that a naturally-ended voice followed by `spu::init()` and the
//! full -> light -> t0a0 replacement produces persistent, data-dependent noise
//! on silicon but not in PSoXide. PA4 keeps the exact timing while varying only
//! explicit voice shutdown and its VBlank delay. A fifth split variant places
//! observation windows between init, light upload, map upload, and readback.

use crate::console_tests::record;
use crate::TimingRecord;
use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};

use crate::payload::{fnv16_bytes, fnv32_words};
use crate::regs::SPU_DELAY;
use crate::{hex2, hex8, spu_dma_read};

include!(concat!(env!("OUT_DIR"), "/bank_layout.rs"));

#[repr(C, align(4))]
struct SampleStaging([u32; 16_400]);

static mut SAMPLE_STAGING: SampleStaging = SampleStaging([0; 16_400]);

/// rec handoff_stage: voices_with_volume_low, blocking_event_vblanks_or_readback_match, endx_low_or_readback_hash_low (two variants x seven stages, 0x4A0-0x4AD)
pub(crate) const HANDOFF_RECORD: u16 = 0x4A0;
const STAGE_COUNT: usize = 7;
const FIELD_COUNT: usize = 10;
const STAGE_FRAMES: [u16; STAGE_COUNT] = [2, 30, 45, 30, 30, 30, 30];
const CAPTURE_FRAMES: [u16; STAGE_COUNT] = [1, 20, 44, 20, 20, 20, 20];
const SPU_SAMPLE_BASE: u32 = 0x1010;
const CALIBRATION_ADDR: u32 = 0x7FF00;
const MENU_VOICE: u8 = 16;
const READBACK_BYTES: usize = 64;

const CALIBRATION_TONE: [u8; 16] = [
    0x00, 0x07, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
];

const ENDX_LO: u32 = psx_hw::spu::BASE + 0x19C;
const ENDX_HI: u32 = psx_hw::spu::BASE + 0x19E;

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

/// The two transitions the run measures. `Baseline` is the game's own order:
/// a naturally ended voice, then `spu::init` and the full, light and map
/// bank replacement. `Safe2` stops the voice first and waits two VBlanks.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Variant {
    Baseline,
    Safe2,
}

impl Variant {
    const fn wait_vblanks(self) -> u8 {
        match self {
            Self::Safe2 => 2,
            Self::Baseline => 0,
        }
    }

    const fn explicit_stop(self) -> bool {
        matches!(self, Self::Safe2)
    }

    const fn short(self) -> &'static str {
        match self {
            Self::Baseline => "BASE",
            Self::Safe2 => "SAFE2",
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
}

impl HandoffProbe {
    pub(crate) const fn new() -> Self {
        Self {
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
        }
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
        tty::println("");
        blocking
    }

    /// Run `variant` to the end, a frame at a time, and return its records:
    /// `0x4A0 + 8 * n + stage` for the n-th variant run.
    pub(crate) fn run(&mut self, variant: Variant, slot: u16) -> [TimingRecord; STAGE_COUNT] {
        self.variant = variant;
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
        spu::init();
        spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::print("hardware-tests: pa4 begin variant=");
        tty::println(self.variant.short());
        let mut tick = 0u32;
        while !self.complete {
            self.update(tick);
            interrupts::wait_vblank();
            tick += 1;
        }
        core::array::from_fn(|stage| {
            let f = &self.records[stage].fields;
            // fields[6] voices with a nonzero volume, [7] blocking-event
            // VBlanks, [2] ENDX; the last stage carries the two readback
            // hashes instead of the VBlank clock samples.
            let last = stage + 1 == STAGE_COUNT;
            record(
                HANDOFF_RECORD + slot * 8 + stage as u16,
                f[6] & 0xFFFF,
                if last { (f[8] == f[9]) as u32 } else { f[7] },
                if last { f[9] & 0xFFFF } else { f[2] & 0xFFFF },
            )
        })
    }

    fn update(&mut self, tick: u32) {
        if self.complete {
            return;
        }
        if self.stage == 0 {
            if self.stage_frame == 15 {
                Voice::start(Voice::V0.mask());
            } else if self.stage_frame == 45 {
                Voice::release(Voice::V0.mask());
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
            return;
        }

        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            self.apply_stage();
        } else {
            self.complete = true;
        }
    }

    fn apply_transition(&mut self) {
        if self.variant.explicit_stop() {
            let menu = Voice::new(MENU_VOICE);
            menu.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::release(menu.mask());
            for _ in 0..self.variant.wait_vblanks() {
                interrupts::wait_vblank();
            }
        }

        spu::init();
        spu::enable_cd_audio(false);
        self.upload_light();
        self.upload_map();
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
        Voice::start(voice.mask());
    }

    fn readback_map(&mut self) {
        assert!(self.map_base != 0, "PA4 map bank not loaded");
        self.observed_hash = stable_read_hash(self.map_base);
    }

    fn capture_stage(&mut self, tick: u32) {
        let stage = self.stage as usize;
        let spucnt = unsafe { psx_io::read_u16(psx_hw::spu::SPUCNT) };
        let spustat = unsafe { psx_io::read_u16(psx_hw::spu::SPUSTAT) };
        let endx_lo = unsafe { psx_io::read_u16(ENDX_LO) };
        let endx_hi = unsafe { psx_io::read_u16(ENDX_HI) };
        let base = psx_hw::spu::BASE + MENU_VOICE as u32 * 16;
        let read = |offset| unsafe { psx_io::read_u16(base + offset) };
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
        let base = psx_hw::spu::BASE + voice * 16;
        let left = unsafe { psx_io::read_u16(base) };
        let right = unsafe { psx_io::read_u16(base + 2) };
        if left != 0 || right != 0 {
            mask |= 1 << voice;
        }
    }
    mask
}

fn stable_read_hash(addr: u32) -> u32 {
    let original_delay = unsafe { psx_io::read_u32(SPU_DELAY) };
    unsafe { psx_io::write_u32(SPU_DELAY, original_delay | 0x0200_0000) };
    for _ in 0..64 {
        core::hint::spin_loop();
    }
    let mut back = [0u32; READBACK_BYTES / 4];
    spu_dma_read(addr, &mut back);
    unsafe { psx_io::write_u32(SPU_DELAY, original_delay) };
    fnv32_words(&back)
}
