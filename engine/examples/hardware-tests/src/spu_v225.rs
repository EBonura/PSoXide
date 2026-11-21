// SPDX-License-Identifier: GPL-2.0-or-later
//! v2.25 SPU questions the 2026-11-20 hardware audit left open: two steps.
//!
//! **SPU FIFO OVERFILL** (records `0xA20`-`0xA24`). `psx_spu`'s paced manual
//! write pushes at most 32 halfwords with the transfer mode at Stop, then
//! starts Manual Write, and rests on two things nothing has measured: that a
//! push in Stop mode is kept, and that the transfer address carries on from
//! one burst to the next. What a console does with a halfword pushed into a
//! full FIFO is not known either (the emulator keeps the FIFO and drops the
//! halfword). Each case clears 256 bytes of SPU RAM at [`FIFO_DEST`], sets the
//! address, pushes `n` halfwords `0xA500 | i` in Stop mode, starts Manual
//! Write, waits for the busy flag and reads the 128 halfwords back:
//!
//! * `0xA20`-`0xA23` (`n` = 32, 33, 48, 64): halfwords that landed (slots
//!   holding a pushed value), the pushed index found in slot 0 (low byte) and
//!   in the last slot that holds one (high byte; `0xFF` none), and the first
//!   slot whose index is not its own (`0xFF` none) with bit 15 set when the
//!   busy wait ran out.
//! * `0xA24` (carry): two bursts of 32 (indexes 0-31, then 32-63) with no
//!   address write between them, same fields. Slot 32 holding index 32 is the
//!   carry `psx_spu` relies on.
//!
//! **SPU KEY, REVERB, IRQ** (records `0xA28`-`0xA2F`).
//!
//! * `0xA28`-`0xA2B` key-on after key-off. A voice at full envelope (a slow
//!   linear attack of about 53 ms, the fastest linear release) is keyed off,
//!   then keyed on again after 0, 768, 1,536 and 3,072 system clocks (none, one,
//!   two and four samples), eight times each; 2 ms later its envelope level
//!   says what happened: about 1,200 (the attack restarted: the key-on took
//!   effect), 0 (the release ran and the key-on was lost), `0x7FFF` (neither
//!   took). Fields: trials that took (bits 0-3), lost (4-7), neither (8-11);
//!   the median level; the largest level.
//! * `0xA2C`-`0xA2D` reverb. The Hall work area (`0xADE0` bytes at the top of
//!   SPU RAM, WipEout's) is filled with a pattern, then `Spu::start_reverb`
//!   clears it (348 pieces of 128 bytes) and programs the 32 registers, depth 0
//!   so nothing is heard. `0xA2C`: the call's result (1 timeout), HBlanks `lo`,
//!   `hi`. `0xA2D`: words of the area not zero afterwards, reverb registers
//!   that read back differently from what was written (of 33, the base
//!   included), SPUCNT's reverb bit after `stop_reverb` (0 cleared).
//! * `0xA2E`-`0xA2F` the IRQ latch. A voice loops a 64-byte tone with the IRQ
//!   address on its second block. `0xA2E`: HBlanks until SPUSTAT bit 6 set
//!   (`0xFFFF` never), `I_STAT` bit 9 then (1 set), SPUSTAT bit 6 after the
//!   acknowledge by clearing SPUCNT bit 6 (0 cleared). `0xA2F`: HBlanks until
//!   it latched again after SPUCNT bit 6 was set again (`0xFFFF` never), the
//!   same with SPUCNT bit 6 left clear (`0xFFFF` expected: no latch while
//!   disabled), 0.
//!
//! Decoder labels: `spu_fifo_push_N`, `spu_fifo_two_bursts`,
//! `spu_kon_after_koff_Nclk`, `spu_reverb_start`, `spu_reverb_check`,
//! `spu_irq_latch`, `spu_irq_latch_again`.

use crate::console_tests::{record, spread};
use crate::ui::{self, Line};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_font::FontAtlas;
use psx_hw::spu::{self as hw, SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};
use psx_io::timers::{self, Timer};
use psx_spu::{Adsr, Pitch, ReverbArea, ReverbInput, ReverbPreset, SpuAddr, Voice, Volume};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const FIFO_RECORD: u16 = 0xA20;
const KEY_RECORD: u16 = 0xA28;
const REVERB_RECORD: u16 = 0xA2C;
const IRQ_RECORD: u16 = 0xA2E;

/// Where the overfill cases write.
const FIFO_DEST: u32 = 0x2000;
/// Halfwords read back after each case.
const FIFO_SLOTS: usize = 128;
/// Pushes of the four overfill cases.
const PUSHES: [u32; 4] = [32, 33, 48, 64];
/// Where the looping tone sits.
const TONE_ADDR: u32 = 0x2800;
/// Timer 1 on the HBlank clock.
const HBLANK_CLOCK: u16 = 0x0100;
/// Key-on spacings after the key-off, in system clocks.
const SPACINGS: [u32; 4] = [0, 768, 1_536, 3_072];
const KEY_TRIALS: usize = 8;
/// Slow linear attack (shift 10, about 53 ms to full), sustain level max.
const KEY_ADSR: Adsr = Adsr {
    lower: 0x280F,
    upper: 0x0000,
};

/// Four ADPCM blocks of a square wave, looping on itself (as `spu_bulk`'s tone).
#[repr(C, align(4))]
struct Tone([u8; 64]);
static TONE: Tone = Tone([
    0x00, 0x04, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x00, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x00, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x03, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
]);

static ZEROS: [u32; 64] = [0; 64];

fn read16(addr: u32) -> u16 {
    // SAFETY: an SPU register read.
    unsafe { psx_io::read_u16(addr) }
}

fn write16(addr: u32, value: u16) {
    // SAFETY: an SPU register store.
    unsafe { psx_io::write_u16(addr, value) }
}

#[optimize(size)]
#[inline(never)]
fn wait_mode(want: u16) {
    let mut guard = 0u32;
    while read16(SPUSTAT) & 0x003F != want & 0x003F && guard < 100_000 {
        guard += 1;
    }
}

/// Start Manual Write, wait for the busy flag to clear (16 reads in a row),
/// back to Stop. True when the wait ran out.
#[optimize(size)]
#[inline(never)]
fn drain(stopped: u16) -> bool {
    write16(SPUCNT, stopped | 0x0010);
    wait_mode(stopped | 0x0010);
    let mut clear = 0u32;
    let mut guard = 0u32;
    while clear < 16 && guard < 100_000 {
        if read16(SPUSTAT) & 0x0400 == 0 {
            clear += 1;
        } else {
            clear = 0;
        }
        guard += 1;
    }
    write16(SPUCNT, stopped);
    wait_mode(stopped);
    clear < 16
}

/// Push indexes `from..to` as `0xA500 | i` in Stop mode, then drain.
#[optimize(size)]
#[inline(never)]
fn burst(stopped: u16, from: u32, to: u32) -> bool {
    for i in from..to {
        write16(TRANSFER_DATA, 0xA500 | i as u16);
    }
    drain(stopped)
}

/// One overfill case: `bursts` pushes of the given index ranges, the address
/// written once before the first.
#[optimize(size)]
#[inline(never)]
fn overfill(bursts: &[(u32, u32)]) -> (u32, u32, u32) {
    // SAFETY: viewing the static zero words as bytes.
    let zeros = unsafe { core::slice::from_raw_parts(ZEROS.as_ptr().cast::<u8>(), 256) };
    crate::probe_upload(SpuAddr::new(FIFO_DEST), zeros);
    let stopped = read16(SPUCNT) & !0x0030;
    write16(SPUCNT, stopped);
    wait_mode(stopped);
    write16(TRANSFER_CTRL, 0x0004);
    write16(TRANSFER_ADDR, (FIFO_DEST / 8) as u16);
    let mut timeout = false;
    for &(from, to) in bursts {
        timeout |= burst(stopped, from, to);
    }
    let mut back = [0u32; FIFO_SLOTS / 2];
    crate::spu_dma_read(FIFO_DEST, &mut back);
    let mut landed = 0u32;
    let mut at_zero = 0xFFu32;
    let mut last = 0xFFu32;
    let mut first_wrong = 0xFFu32;
    for slot in 0..FIFO_SLOTS {
        let half = (back[slot / 2] >> ((slot % 2) * 16)) & 0xFFFF;
        let index = if half >> 8 == 0xA5 { half & 0xFF } else { 0xFF };
        if index != 0xFF {
            landed += 1;
            last = index;
            if slot == 0 {
                at_zero = index;
            }
        }
        if index != slot as u32 && first_wrong == 0xFF && index != 0xFF {
            first_wrong = slot as u32;
        }
    }
    (landed, at_zero | (last << 8), first_wrong | (u32::from(timeout) << 15))
}

/// `SPU FIFO OVERFILL`.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run_overfill(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    let mut line = Line::new();
    line.s("LANDED OF PUSHED:");
    for (case, &n) in PUSHES.iter().enumerate() {
        let id = FIFO_RECORD + case as u16;
        crate::bounds::record_start(id);
        let (landed, b, c) = overfill(&[(0, n)]);
        push_timing_record(records, next, record(id, landed, b, c));
        line.s(" ").u(landed).s("/").u(n);
    }
    ui::live_line(font, 0, line.as_str(), (220, 224, 230));
    let id = FIFO_RECORD + 4;
    crate::bounds::record_start(id);
    let (landed, b, c) = overfill(&[(0, 32), (32, 64)]);
    push_timing_record(records, next, record(id, landed, b, c));
    let mut line = Line::new();
    line.s("TWO BURSTS OF 32, NO ADDRESS WRITE: ").u(landed).s(" LANDED");
    ui::live_line(font, 1, line.as_str(), (220, 224, 230));
}

fn level(voice: Voice) -> u32 {
    u32::from(read16(hw::voice::base(u32::from(voice.index())) + hw::voice::ADSR_LEVEL))
}

/// Spin `clocks` system clocks on Timer 2.
#[optimize(size)]
#[inline(never)]
fn wait_clocks(clocks: u32) {
    timers::set_mode(Timer::Timer2, 0);
    timers::set_counter(Timer::Timer2, 0);
    while u32::from(timers::counter(Timer::Timer2)) < clocks {}
}

/// Wait `lines` HBlanks on Timer 1.
#[optimize(size)]
#[inline(never)]
fn wait_lines(lines: u32) {
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let mut guard = 0u32;
    while u32::from(timers::counter(Timer::Timer1)) < lines && guard < 20_000_000 {
        guard += 1;
    }
}

#[optimize(size)]
#[inline(never)]
fn key_spacing(voice: Voice, spacing: u32) -> (u32, u32, u32) {
    let mut levels = [0u32; KEY_TRIALS];
    let (mut took, mut lost, mut neither) = (0u32, 0u32, 0u32);
    for value in levels.iter_mut() {
        voice.configure_sample(SpuAddr::new(TONE_ADDR), 44_100, Volume::SILENCE, KEY_ADSR);
        voice.set_loop_addr(SpuAddr::new(TONE_ADDR));
        Voice::start(voice.mask());
        // About 76 ms: the attack is done.
        wait_lines(1_200);
        Voice::release(voice.mask());
        if spacing != 0 {
            wait_clocks(spacing);
        }
        Voice::start(voice.mask());
        // About 2 ms.
        wait_lines(32);
        let now = level(voice);
        *value = now;
        if now >= 0x7000 {
            neither += 1;
        } else if now == 0 {
            lost += 1;
        } else {
            took += 1;
        }
        Voice::release(voice.mask());
        wait_lines(16);
    }
    let (_, median, max) = spread(&mut levels);
    (took | lost << 4 | neither << 8, median, max)
}

/// `SPU KEY, REVERB, IRQ`.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run_key_reverb_irq(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    crate::probe_upload(SpuAddr::new(TONE_ADDR), &TONE.0);
    // (a) Key-on after key-off.
    let voice = Voice::V1;
    let mut line = Line::new();
    line.s("KON AFTER KOFF, TOOK OF 8:");
    for (case, &spacing) in SPACINGS.iter().enumerate() {
        let id = KEY_RECORD + case as u16;
        crate::bounds::record_start(id);
        let (counts, median, max) = key_spacing(voice, spacing);
        push_timing_record(records, next, record(id, counts, median, max));
        line.s(" ").u(counts & 0xF);
    }
    ui::live_line(font, 2, line.as_str(), (220, 224, 230));

    // (b) Reverb: fill the work area, let start_reverb clear it.
    crate::bounds::record_start(REVERB_RECORD);
    let preset = ReverbPreset::HALL;
    let area = ReverbArea::top_of_ram(preset.work_bytes);
    let pattern = [0x5A5A_5A5Au32; 256];
    // SAFETY: viewing the pattern words as bytes.
    let bytes = unsafe { core::slice::from_raw_parts(pattern.as_ptr().cast::<u8>(), 1024) };
    let mut at = area.base();
    while at < area.end() {
        let len = (area.end() - at).min(1024) as usize;
        crate::probe_upload(SpuAddr::new(at), &bytes[..len]);
        at += len as u32;
    }
    // SAFETY: the suite owns the SPU channel while the step runs.
    let mut spu = psx_spu::Spu::new(unsafe { psx_io::periph::SpuDma::steal() });
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let result = spu.start_reverb(area, &preset, ReverbInput::new(0, false), 0);
    // Under four seconds, so the 16-bit count does not wrap.
    let lines = u32::from(timers::counter(Timer::Timer1));
    let mut wrong = 0u32;
    for (k, &value) in preset.registers.iter().enumerate() {
        if read16(hw::REVERB_COEFFICIENTS + 2 * k as u32) != value {
            wrong += 1;
        }
    }
    if read16(hw::REVERB_WORK_BASE) != (area.base() / 8) as u16 {
        wrong += 1;
    }
    let mut nonzero = 0u32;
    let mut back = [0u32; 256];
    let mut at = area.base();
    while at < area.end() {
        let words = ((area.end() - at) / 4).min(256) as usize;
        crate::spu_dma_read(at, &mut back[..words]);
        nonzero += back[..words].iter().filter(|&&w| w != 0).count() as u32;
        at += words as u32 * 4;
    }
    psx_spu::stop_reverb();
    let reverb_bit = u32::from(read16(SPUCNT) & 0x0080 != 0);
    let _ = spu.release();
    push_timing_record(
        records,
        next,
        record(REVERB_RECORD, u32::from(result.is_err()), lines & 0xFFFF, lines >> 16),
    );
    push_timing_record(records, next, record(REVERB_RECORD + 1, nonzero, wrong, reverb_bit));
    let mut line = Line::new();
    line.s("REVERB CLEAR: ")
        .u(nonzero)
        .s(" WORDS LEFT, ")
        .u(wrong)
        .s(" REGS DIFFER, ")
        .u(lines * 1000 / 15_734)
        .s(" MS");
    let ok = nonzero == 0 && wrong == 0 && result.is_ok();
    ui::live_line(font, 3, line.as_str(), if ok { (120, 230, 120) } else { (255, 110, 90) });

    // (c) The IRQ latch.
    crate::bounds::record_start(IRQ_RECORD);
    let voice = Voice::V2;
    let base = read16(SPUCNT) & !0x0040;
    write16(SPUCNT, base);
    psx_spu::set_irq_address(SpuAddr::new(TONE_ADDR + 16));
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::SPU);
    voice.configure_sample(SpuAddr::new(TONE_ADDR), 44_100, Volume::SILENCE, Adsr::sample());
    voice.set_loop_addr(SpuAddr::new(TONE_ADDR));
    voice.set_pitch(Pitch::raw(0x1000));
    write16(SPUCNT, base | 0x0040);
    Voice::start(voice.mask());
    let first = latch_lines();
    let i_stat = u32::from(psx_io::irq::pending() & (1 << psx_hw::irq::source::SPU) != 0);
    write16(SPUCNT, base);
    wait_mode(base);
    let after_ack = u32::from(read16(SPUSTAT) & 0x0040 != 0);
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::SPU);
    let disabled = latch_lines();
    write16(SPUCNT, base | 0x0040);
    let again = latch_lines();
    write16(SPUCNT, base);
    Voice::release(voice.mask());
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::SPU);
    push_timing_record(records, next, record(IRQ_RECORD, first, i_stat, after_ack));
    push_timing_record(records, next, record(IRQ_RECORD + 1, again, disabled, 0));
    let mut line = Line::new();
    line.s("SPU IRQ: FIRST ")
        .u(first)
        .s(" LINES, ACK CLEARS ")
        .s(if after_ack == 0 { "YES" } else { "NO" })
        .s(", AGAIN ")
        .u(again);
    ui::live_line(font, 4, line.as_str(), (220, 224, 230));
}

/// HBlanks until SPUSTAT bit 6 is set, up to about 50 ms; `0xFFFF` never.
#[optimize(size)]
#[inline(never)]
fn latch_lines() -> u32 {
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let mut guard = 0u32;
    while guard < 4_000_000 {
        let lines = u32::from(timers::counter(Timer::Timer1));
        if read16(SPUSTAT) & 0x0040 != 0 {
            return lines;
        }
        if lines > 800 {
            break;
        }
        guard += 1;
    }
    0xFFFF
}
