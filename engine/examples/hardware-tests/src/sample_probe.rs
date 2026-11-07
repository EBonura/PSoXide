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
use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, Adsr, SpuAddr, Voice, Volume};

use crate::console_tests::record;
use crate::{hex2, hex8, spu_dma_read, TimingRecord};

/// The launcher's own cooked browse blip, byte for byte. The select
/// blip (pickup_coin) stays out: at 7 KiB it alone overflowed the
/// playtest boot-EXE budget, and the reported bug is the browse blip.
static BEEP_PSAU: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/ui_beep.psau");

/// rec sb1_audit: blocks_or_loop_starts, flags_or_and_last_or_readback_low, first_end_block_or_readback_high (0x430 audit, 0x431 upload readback)
pub(crate) const SB1_AUDIT: u16 = 0x430;
/// rec sb1_stage: envelope_at_frame_32, envelope_at_frame_100, endx_bit_per_checkpoint (five records, 0x432-0x436)
pub(crate) const SB1_STAGE: u16 = 0x432;
const STAGE_COUNT: usize = 6;
const FIELD_COUNT: usize = 9;
/// Frames each stage runs. Stage 0 is the silent audit.
const STAGE_FRAMES: [u16; STAGE_COUNT] = [4, 130, 130, 130, 130, 130];
/// Frames within a stage at which the envelope/ENDX checkpoints sample.
const CHECKPOINTS: [u16; 8] = [2, 8, 16, 32, 64, 100, 120, 128];

/// SPU layout: the blip at the first free SPU address, like the SDK.
const SPU_SAMPLE_BASE: u32 = 0x1010;
/// The launcher's browse voice, mirrored exactly.
const BEEP_VOICE: u8 = 0;

const ENDX_LO: u32 = psx_hw::spu::BASE + 0x19C;

/// Readback staging for the audit: ui_beep is 2 KiB of psau, so its
/// ADPCM fits with room to spare.
static mut READBACK: [u32; 1024] = [0; 1024];

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
        }
    }

    pub(crate) fn start(&mut self) {
        self.stage = 0;
        self.stage_frame = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.records = [StageRecord::empty(); STAGE_COUNT];
        spu::init();
        spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
        spu::enable_cd_audio(false);
        self.apply_stage();
        tty::println("hardware-tests: sb1 begin ui-sample probe");
    }

    /// Run the audit and the five keyed stages, a frame at a time, and
    /// return the records: `0x430` the flag audit, `0x431` its upload
    /// readback, `0x432`-`0x436` the stages.
    pub(crate) fn run(&mut self) -> [TimingRecord; 7] {
        self.start();
        let mut tick = 0u32;
        while !self.complete {
            self.update(tick);
            interrupts::wait_vblank();
            tick += 1;
        }
        self.records_out()
    }

    fn records_out(&self) -> [TimingRecord; 7] {
        let audit = self.beep_audit;
        let mut out = [
            record(
                SB1_AUDIT,
                audit.blocks as u32,
                ((audit.or_flags as u32) << 8) | audit.last_flags as u32,
                audit.first_end as u32,
            ),
            record(
                SB1_AUDIT + 1,
                audit.loop_starts as u32,
                self.beep_readback_fnv & 0xFFFF,
                self.beep_readback_fnv >> 16,
            ),
            record(SB1_AUDIT, 0, 0, 0),
            record(SB1_AUDIT, 0, 0, 0),
            record(SB1_AUDIT, 0, 0, 0),
            record(SB1_AUDIT, 0, 0, 0),
            record(SB1_AUDIT, 0, 0, 0),
        ];
        for stage in 1..STAGE_COUNT {
            let fields = &self.records[stage].fields;
            // fields[1 + slot] = envelope << 16 | ENDX bit at checkpoint `slot`.
            let mut endx = 0u32;
            for slot in 0..CHECKPOINTS.len() {
                endx |= (fields[1 + slot] & 1) << slot;
            }
            out[1 + stage] = record(
                SB1_STAGE + stage as u16 - 1,
                fields[1 + 3] >> 16,
                fields[1 + 5] >> 16,
                endx,
            );
        }
        out
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
                    Voice::start(Voice::new(BEEP_VOICE).mask());
                }
            }
            3 if self.stage_frame == 60 => {
                Voice::release(Voice::new(BEEP_VOICE).mask());
            }
            _ => {}
        }

        if let Some(slot) = CHECKPOINTS.iter().position(|&f| f == self.stage_frame) {
            let voice = BEEP_VOICE;
            let base = psx_hw::spu::BASE + voice as u32 * 16;
            let env = unsafe { psx_io::read_u16(base + 12) };
            let endx = unsafe { psx_io::read_u16(ENDX_LO) };
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

        Voice::release(Voice::new(BEEP_VOICE).mask());
        self.stage_frame = 0;
        if stage + 1 < STAGE_COUNT {
            self.stage += 1;
            self.records[self.stage as usize].fields[0] =
                ((self.stage as u32) << 24) | (tick & 0x00FF_FFFF);
            self.apply_stage();
        } else {
            self.complete = true;
        }
    }

    fn apply_stage(&mut self) {
        match self.stage {
            0 => self.audit(),
            1..=3 => self.key(BEEP_VOICE, self.beep_addr, self.beep_rate, Adsr::sample()),
            4 => self.key(
                BEEP_VOICE,
                self.beep_addr,
                self.beep_rate,
                Adsr::percussive(),
            ),
            5 => self.key(
                BEEP_VOICE,
                self.beep_addr,
                self.beep_rate,
                Adsr::default_tone(),
            ),
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
        Voice::start(voice.mask());
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
