// SPDX-License-Identifier: GPL-2.0-or-later
//! XA MEASURE: what XA music costs on a console, from objective signals.
//!
//! The emulator models none of these times, and the numbers decide how a game
//! can use XA (a launcher that skips songs, a level load that interrupts the
//! music, a rhythm game that needs the song clock). Every case listens with
//! the SPU's CD capture buffer (SPU RAM 0 to 0x1FF, the left CD input), as
//! the CD route and CD-DA handoff cases do, so "the music is back" is a number
//! and nobody has to listen. Two of the generated test songs are told apart
//! from the zero crossings of 256 captured samples: the pad (channel 0, a low
//! chord) has few, the whistle (channel 3, 1 to 1.8 kHz) many.
//!
//! * START: `Player::play` from a paused drive to the head being seen in the
//!   file and to sound in the capture buffer, five times.
//! * SWITCH: a Setfilter alone, mid-stream, to the other song (no seek), from
//!   the command to the other song being heard, six times; and the same switch
//!   made by starting the other song from the top, three times.
//! * AFTER A DATA READ: a 64-sector data read (double speed, the CD stream
//!   test's region) interrupts a song, then three ways back, three times
//!   each, timed from the last data sector to sound: the XA resume
//!   (Demute, Setmode, Setfilter, Setloc to the head GetlocP gave at the
//!   pause, ReadS: the same commands as `Player::resume` of the SDK branch
//!   `feat/xa-clock-resume-2026-10-15`, sent here by hand because this suite
//!   builds against an older SDK pin), a fresh XA start, and CD-DA Play.
//! * CLOCK: `HWCLOCK.XA`, one 190 s song, plays for 180 s on a drive polled
//!   twice a second. The head's progress (GetlocP, one sector is 13.3 ms) is
//!   compared with the system clock (Timer 2, read in a tight loop, so it has
//!   no 15.5 ms wrap problem) at 60, 120 and 180 s. The drive's sector rate
//!   comes from its own oscillator, the console's clocks from the console's,
//!   so the difference is what a game's frame counter, which counts on the
//!   console's clock, has against its music. HBlank (Timer 1) only places each
//!   Timer 2 reading among its wraps, and its line length is measured on the
//!   side: every conversion in this case uses the system clock.
//!
//! Every wait counts loop iterations (`bounds::scale`), never a timer; a wait
//! that runs out is recorded as `0xFFFF`, the song is stopped and the case goes
//! on. The audio is stopped and the CD volume restored at the end.
//!
//! Records from `0x320` (fields min, median, max of the trials unless noted;
//! times in milliseconds, "x10" in tenths, "signed" is two's complement):
//! 0x320: start, play call to sound;
//! 0x321: start, play call to the head seen in the file;
//! 0x322: start, the `play` call itself (it blocks for five commands), x10;
//! 0x323: Setfilter to the other song heard;
//! 0x324: the Setfilter command's own response time, x10;
//! 0x325: switch by restarting the other song, call to sound;
//! 0x326: XA resume after the data read, last data sector to sound;
//! 0x327: XA restart after the data read, same;
//! 0x328: CD-DA Play after the data read, same;
//! 0x329: the data read itself (64 sectors), request to last sector;
//! 0x32A: where the resumed song continued: head after the resume minus the
//!   head at the pause, signed ms;
//! 0x32B: trials that worked: starts (of 5), filter switches (of 6), XA
//!   resumes (of 3);
//! 0x32C: head progress minus system-clock time since the start of the song,
//!   signed ms, at 60 s, 120 s and 180 s (0xFFFF if not reached; a positive
//!   value means the drive's sectors run faster than the console's clock);
//! 0x32D: clock run flags (bit 0 `HWCLOCK.XA` found, 1 play refused, 2 head
//!   seen in the file, 3 ran the full 180 s, 4 `HWSONGS.XA` missing), seconds
//!   run, polls;
//! 0x32E: distinct head positions seen, longest time one stood still while the
//!   song streamed (ms), system clocks per HBlank line over the run, in
//!   hundredths, less 2000 (21723 means 2172.23; the line is 3413 video clocks
//!   at 11/7 of the system clock, 2171.9, if the video clock is locked to it).

use crate::bounds;
use crate::console_tests::{bit, record, spread, XA_MEASURE_COUNT, XA_MEASURE_RECORD};
use crate::xa_loop::{find_file, Clock, FILE_NUMBER};
use crate::{spu_dma_read, ui, TimingRecord};
use core::ptr::addr_of_mut;
use psx_font::FontAtlas;
use psx_hw::cd::{
    CMD_READS, CMD_SETFILTER, MODE_AUTO_PAUSE, MODE_CDDA, MODE_XA_ADPCM, MODE_XA_FILTER,
};
use psx_io::cd::xa::{File, Player};
use psx_io::cd::PlayPosition;
use psx_io::periph::Cd;
use psx_io::timers::{self, Timer};
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::interrupts;
use psx_spu::{self as spu, CdVolume, Volume};

/// Song files on the disc: the four tone songs, and the long one.
const CLOCK_FILE: &str = "HWCLOCK.XA";
const CLOCK_FILE_NUMBER: u8 = 2;
const SONGS_FILE: &str = "HWSONGS.XA";
const LOW_CHANNEL: u8 = 0;
const HIGH_CHANNEL: u8 = 3;
/// `CDTEST.BIN`, the deterministic read region every hardware-test disc has.
const CDTEST_LBA: u32 = 564;
const DATA_SECTORS: u32 = 64;
/// Per-command spin budget, as the rest of the suite uses.
const SPINS: u32 = 200_000;
/// First CD-DA track after the data track: the synthetic tone.
const TONE_TRACK: u8 = 2;
const CDDA_MODE: u8 = MODE_CDDA | MODE_AUTO_PAUSE;

const START_TRIALS: usize = 5;
const SWITCH_TRIALS: usize = 6;
const RESTART_TRIALS: usize = 3;
const READ_TRIALS: usize = 3;
/// Seconds the clock case compares, and the longest it runs.
const DRIFT_MARKS: [u32; 3] = [60, 120, 180];
const DRIFT_SECONDS: u32 = 180;
/// The drive is polled twice a second while the clock case runs.
const POLL_TICKS: u32 = SYS_TICKS_PER_SECOND / 2;
/// Passes of the clock case's loop it is allowed, far more than the 180 s need.
const DRIFT_ITERATIONS: u32 = 600_000_000;
/// Frames the clock case gives the head to appear in the file.
const STREAM_FRAMES: u32 = 600;
/// Iteration bounds (each iteration is one capture read, about half a
/// millisecond, or one frame where it says so).
const AUDIO_POLLS: u32 = 4000;
const SILENCE_POLLS: u32 = 300;
/// Spins for one VBlank wait.
const VBLANK_SPINS: u32 = 400_000;

/// Peak-to-peak of the capture buffer that counts as audio.
const SIGNAL: u32 = 1000;
/// The CD-left capture bank: 256 samples, 128 words.
const LEFT_WORDS: usize = 128;
/// Sign changes in the bank at or above which the whistle is heard rather
/// than the pad (the pad has 4 or 5 with the ring's seam, the whistle 20 or
/// more).
const HIGH_CROSSINGS: u32 = 10;

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

// ------------------------------------------------------------------ hearing

#[derive(Copy, Clone, PartialEq, Eq)]
enum Heard {
    Silent,
    Low,
    High,
}

/// What the SPU's CD input is carrying right now.
#[inline(never)]
fn heard() -> Heard {
    let mut words = [0u32; LEFT_WORDS];
    spu_dma_read(0, &mut words);
    let (mut lo, mut hi) = (i16::MAX, i16::MIN);
    let mut crossings = 0u32;
    let mut was_negative = None;
    for word in words {
        for half in [word as i16, (word >> 16) as i16] {
            lo = lo.min(half);
            hi = hi.max(half);
            let negative = half < 0;
            if was_negative.is_some_and(|before| before != negative) {
                crossings += 1;
            }
            was_negative = Some(negative);
        }
    }
    if (i32::from(hi) - i32::from(lo)) as u32 <= SIGNAL {
        Heard::Silent
    } else if crossings >= HIGH_CROSSINGS {
        Heard::High
    } else {
        Heard::Low
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Want {
    Any,
    Low,
    High,
}

impl Want {
    fn is(self, heard: Heard) -> bool {
        match self {
            Want::Any => heard != Heard::Silent,
            Want::Low => heard == Heard::Low,
            Want::High => heard == Heard::High,
        }
    }
}

/// Wait for `want` in the capture buffer, running `between` before each look;
/// the time it was first heard, or `None` if the bound ran out.
#[inline(never)]
fn wait_heard(clock: &mut Watch, want: Want, between: &mut dyn FnMut(&mut Watch)) -> Option<u32> {
    for _ in 0..bounds::scale(AUDIO_POLLS) {
        between(clock);
        if want.is(heard()) {
            return Some(clock.ticks());
        }
    }
    None
}

/// Pause the drive and wait for the capture buffer to go quiet, so the next
/// trial cannot hear the last one.
#[inline(never)]
fn quiet() -> bool {
    let _ = psx_io::cd::try_pause_until_complete(SPINS);
    for _ in 0..bounds::scale(SILENCE_POLLS) {
        if heard() == Heard::Silent {
            return true;
        }
        let _ = interrupts::try_wait_vblank(VBLANK_SPINS);
    }
    false
}

/// A player on a drive nothing else is using.
fn player() -> Player {
    // SAFETY: the polled reader has stopped and nothing else drives the
    // controller until the player is released.
    let mut player = Player::new(unsafe { Cd::steal() });
    player.set_volume(0x80, 0x80);
    player
}

// ---------------------------------------------------------------- the drive

/// The absolute LBA the head is over (GetlocP), if the drive answers.
#[inline(never)]
fn head_lba() -> Option<u32> {
    let response = psx_io::cd::try_play_position(SPINS)?;
    let position = PlayPosition::parse(&response)?;
    let frames = (u32::from(position.absolute_min) * 60 + u32::from(position.absolute_sec)) * 75
        + u32::from(position.absolute_frame);
    frames.checked_sub(150)
}

/// The commands `Player::resume` sends: everything again, aimed at `lba`.
#[inline(never)]
fn raw_resume(number: u8, channel: u8, lba: u32) -> bool {
    psx_io::cd::try_unmute(SPINS).is_some()
        && psx_io::cd::try_set_mode(MODE_XA_ADPCM | MODE_XA_FILTER, SPINS).is_some()
        && psx_io::cd::try_command(CMD_SETFILTER, &[number, channel], SPINS).is_some()
        && psx_io::cd::try_set_target_lba(lba, SPINS).is_some()
        && psx_io::cd::try_command(CMD_READS, &[], SPINS).is_some()
}

/// Read `DATA_SECTORS` sectors of the test region at double speed. How many
/// arrived, and the time from the request to the last one.
#[inline(never)]
fn data_read(clock: &mut Watch) -> (u32, u32) {
    // SAFETY: single-threaded use of the reader and its sector buffer, with
    // nothing else driving the controller.
    let (reader, sector) = unsafe { (&mut *addr_of_mut!(READER), &mut *addr_of_mut!(SECTOR)) };
    let t0 = clock.ticks();
    let mut got = 0;
    if reader.prepare() && reader.start_read(CDTEST_LBA) {
        while got < DATA_SECTORS && reader.read_sector(sector) {
            got += 1;
        }
    }
    reader.stop();
    (got, clock.ticks().wrapping_sub(t0))
}

fn start_tone() -> bool {
    psx_io::cd::try_set_mode(CDDA_MODE, SPINS).is_some()
        && psx_io::cd::try_unmute(SPINS).is_some()
        && psx_io::cd::try_play_track(TONE_TRACK, SPINS).is_some()
}

// ----------------------------------------------------------------- records

/// System clock / 8, the rate Timer 2 counts at: 4,233,600 Hz.
const SYS_TICKS_PER_SECOND: u32 = 4_233_600;
/// System-clock ticks per HBlank line, times 100. A line is 3413 video clocks
/// at 11/7 of the system clock: 2171.9 system clocks, 271.49 of these ticks.
/// Only used to place a Timer 2 reading among its 15.5 ms wraps, never to
/// convert a result.
const TICKS_PER_LINE_X100: u32 = 27_149;

/// Time on the system clock: Timer 2 counting system clock / 8, widened to 32
/// bits (1,014 s). Timer 2 wraps every 15.5 ms, which a blocking drive call
/// outlasts, so each reading is placed among its wraps by the HBlank counter
/// (Timer 1, 4.2 s to a wrap) read since the last one: the HBlank count
/// only has to be right to within 7 ms over the interval, which it is by
/// better than a tenth of a percent. Read it at least every four seconds.
struct Watch {
    lines: Clock,
    last_lines: u32,
    last_total: u32,
}

impl Watch {
    fn start() -> Self {
        timers::set_mode(Timer::Timer2, 0x0200);
        timers::set_counter(Timer::Timer2, 0);
        Self {
            lines: Clock::start(),
            last_lines: 0,
            last_total: 0,
        }
    }

    #[inline(never)]
    fn ticks(&mut self) -> u32 {
        let lines = self.lines.ticks();
        let fine = u32::from(timers::counter(Timer::Timer2));
        let expected = self.last_total.wrapping_add(
            lines
                .wrapping_sub(self.last_lines)
                .wrapping_mul(TICKS_PER_LINE_X100)
                / 100,
        );
        let mut total = (expected & !0xFFFF) | fine;
        if total.wrapping_add(0x8000) < expected {
            total = total.wrapping_add(0x1_0000);
        } else if total > expected.wrapping_add(0x8000) {
            total = total.wrapping_sub(0x1_0000);
        }
        self.last_lines = lines;
        self.last_total = total;
        total
    }
}

/// Milliseconds of a span of system-clock / 8 ticks.
fn sys_ms(ticks: u32) -> u32 {
    ticks / SYS_TICKS_PER_SECOND * 1000 + ticks % SYS_TICKS_PER_SECOND * 1000 / SYS_TICKS_PER_SECOND
}

/// Tenths of a millisecond, for spans under ten seconds.
fn sys_ms10(ticks: u32) -> u32 {
    ticks.min(42_336_000) * 100 / 42_336
}

fn signed(value: i32) -> u32 {
    value.clamp(-32768, 32767) as i16 as u16 as u32
}

/// Trial results, kept by value.
struct Samples<const N: usize> {
    values: [u32; N],
    count: usize,
}

impl<const N: usize> Samples<N> {
    const fn new() -> Self {
        Self {
            values: [0; N],
            count: 0,
        }
    }

    fn add(&mut self, value: u32) {
        if self.count < N {
            self.values[self.count] = value;
            self.count += 1;
        }
    }

    /// Min, median and max, or `0xFFFF` three times if nothing was measured.
    fn spread(&mut self) -> (u32, u32, u32) {
        if self.count == 0 {
            (0xFFFF, 0xFFFF, 0xFFFF)
        } else {
            spread(&mut self.values[..self.count])
        }
    }
}

fn triple(id_offset: u16, (a, b, c): (u32, u32, u32)) -> TimingRecord {
    record(XA_MEASURE_RECORD + id_offset, a, b, c)
}

// ------------------------------------------------------------------ the run

/// The case's progress for the step's bar.
struct Progress<'a> {
    font: &'a FontAtlas,
    done: usize,
}

impl Progress<'_> {
    const TOTAL: usize =
        START_TRIALS + SWITCH_TRIALS + RESTART_TRIALS + 3 * READ_TRIALS + DRIFT_SECONDS as usize;

    fn show(&self, name: &str) {
        ui::detail(self.font, "XA", name);
        ui::sub(self.done, Self::TOTAL);
    }

    fn step(&mut self, name: &str) {
        self.show(name);
        self.done += 1;
    }
}

/// Everything the case measures.
#[inline(never)]
pub(crate) fn run(font: &FontAtlas) -> [TimingRecord; XA_MEASURE_COUNT] {
    // SAFETY: plain SPU register reads; restored below.
    let (spucnt, cd_left, cd_right) = unsafe {
        (
            psx_io::read_u16(psx_hw::spu::SPUCNT),
            psx_io::read_u16(0x1F80_1DB0),
            psx_io::read_u16(0x1F80_1DB2),
        )
    };
    spu::init();
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);
    let mut clock;
    let mut progress = Progress { font, done: 0 };

    let songs = find_file(SONGS_FILE, FILE_NUMBER);
    let long = find_file(CLOCK_FILE, CLOCK_FILE_NUMBER);
    // `bounds::record_start` looks at the pad, which reprograms the root
    // counters, so each group starts its clocks again after it.

    // START
    bounds::record_start(XA_MEASURE_RECORD);
    clock = Watch::start();
    let mut start_audio = Samples::<START_TRIALS>::new();
    let mut start_head = Samples::<START_TRIALS>::new();
    let mut play_call = Samples::<START_TRIALS>::new();
    if let Some(file) = songs {
        for _ in 0..START_TRIALS {
            progress.step("START FROM PAUSED");
            quiet();
            let mut p = player();
            let t0 = clock.ticks();
            let refused = p.play(file.song(LOW_CHANNEL), false).is_err();
            play_call.add(sys_ms10(clock.ticks().wrapping_sub(t0)));
            if !refused {
                let mut head = None;
                let heard_at = wait_heard(&mut clock, Want::Low, &mut |c| {
                    p.poll();
                    if head.is_none() && p.is_streaming() {
                        head = Some(c.ticks());
                    }
                });
                if let Some(at) = heard_at {
                    start_audio.add(sys_ms(at.wrapping_sub(t0)));
                }
                if let Some(at) = head {
                    start_head.add(sys_ms(at.wrapping_sub(t0)));
                }
            }
            p.release();
        }
    }
    let starts_ok = start_audio.count as u32;

    // SWITCH
    bounds::record_start(XA_MEASURE_RECORD + 3);
    clock = Watch::start();
    let mut by_filter = Samples::<SWITCH_TRIALS>::new();
    let mut filter_ack = Samples::<SWITCH_TRIALS>::new();
    let mut by_restart = Samples::<RESTART_TRIALS>::new();
    if let Some(file) = songs {
        quiet();
        let mut p = player();
        if p.play(file.song(LOW_CHANNEL), true).is_ok()
            && wait_heard(&mut clock, Want::Low, &mut |_| {
                p.poll();
            })
            .is_some()
        {
            let mut channel = LOW_CHANNEL;
            for _ in 0..SWITCH_TRIALS {
                progress.step("SETFILTER SWITCH");
                let (to, want) = if channel == LOW_CHANNEL {
                    (HIGH_CHANNEL, Want::High)
                } else {
                    (LOW_CHANNEL, Want::Low)
                };
                let t0 = clock.ticks();
                let acked =
                    psx_io::cd::try_command(CMD_SETFILTER, &[FILE_NUMBER, to], SPINS).is_some();
                let t_ack = clock.ticks();
                if acked {
                    filter_ack.add(sys_ms10(t_ack.wrapping_sub(t0)));
                }
                let heard_at = wait_heard(&mut clock, want, &mut |_| {
                    p.poll();
                });
                match heard_at {
                    Some(at) if acked => {
                        by_filter.add(sys_ms(at.wrapping_sub(t0)));
                        channel = to;
                    }
                    _ => break,
                }
            }
            for _ in 0..RESTART_TRIALS {
                progress.step("RESTART SWITCH");
                let (to, want) = if channel == LOW_CHANNEL {
                    (HIGH_CHANNEL, Want::High)
                } else {
                    (LOW_CHANNEL, Want::Low)
                };
                let t0 = clock.ticks();
                if p.play(file.song(to), true).is_err() {
                    break;
                }
                match wait_heard(&mut clock, want, &mut |_| {
                    p.poll();
                }) {
                    Some(at) => {
                        by_restart.add(sys_ms(at.wrapping_sub(t0)));
                        channel = to;
                    }
                    None => break,
                }
            }
        }
        p.release();
    }
    let switches_ok = by_filter.count as u32;

    // AFTER A DATA READ
    bounds::record_start(XA_MEASURE_RECORD + 6);
    clock = Watch::start();
    let mut resume_audio = Samples::<READ_TRIALS>::new();
    let mut restart_audio = Samples::<READ_TRIALS>::new();
    let mut cdda_audio = Samples::<READ_TRIALS>::new();
    let mut read_time = Samples::<{ 3 * READ_TRIALS }>::new();
    let mut moved = Samples::<READ_TRIALS>::new();
    if let Some(file) = songs {
        for _ in 0..READ_TRIALS {
            // The XA resume.
            progress.step("XA RESUME AFTER READ");
            quiet();
            let mut p = player();
            if p.play(file.song(LOW_CHANNEL), true).is_ok()
                && wait_heard(&mut clock, Want::Low, &mut |_| {
                    p.poll();
                })
                .is_some()
            {
                let _ = psx_io::cd::try_pause_until_complete(SPINS);
                let paused = head_lba();
                let (got, took) = data_read(&mut clock);
                if got == DATA_SECTORS {
                    read_time.add(sys_ms(took));
                }
                let t_read = clock.ticks();
                if let Some(at) = paused {
                    if got == DATA_SECTORS && raw_resume(FILE_NUMBER, LOW_CHANNEL, at) {
                        if let Some(heard_at) = wait_heard(&mut clock, Want::Low, &mut |_| {}) {
                            resume_audio.add(sys_ms(heard_at.wrapping_sub(t_read)));
                            if let Some(now) = head_lba() {
                                moved.add(signed((now as i32 - at as i32) * 1000 / 75));
                            }
                        }
                    }
                }
            }
            p.release();

            // A fresh XA start in the same place.
            progress.step("XA RESTART AFTER READ");
            quiet();
            let mut p = player();
            if p.play(file.song(LOW_CHANNEL), true).is_ok()
                && wait_heard(&mut clock, Want::Low, &mut |_| {
                    p.poll();
                })
                .is_some()
            {
                let _ = psx_io::cd::try_pause_until_complete(SPINS);
                let (got, took) = data_read(&mut clock);
                if got == DATA_SECTORS {
                    read_time.add(sys_ms(took));
                }
                let t_read = clock.ticks();
                if got == DATA_SECTORS && p.play(file.song(LOW_CHANNEL), true).is_ok() {
                    if let Some(at) = wait_heard(&mut clock, Want::Low, &mut |_| {
                        p.poll();
                    }) {
                        restart_audio.add(sys_ms(at.wrapping_sub(t_read)));
                    }
                }
            }
            p.release();

            // CD-DA Play after the same read.
            progress.step("CD-DA PLAY AFTER READ");
            quiet();
            if start_tone() && wait_heard(&mut clock, Want::Any, &mut |_| {}).is_some() {
                let _ = psx_io::cd::try_pause_until_complete(SPINS);
                let (got, took) = data_read(&mut clock);
                if got == DATA_SECTORS {
                    read_time.add(sys_ms(took));
                }
                let t_read = clock.ticks();
                if got == DATA_SECTORS && start_tone() {
                    if let Some(at) = wait_heard(&mut clock, Want::Any, &mut |_| {}) {
                        cdda_audio.add(sys_ms(at.wrapping_sub(t_read)));
                    }
                }
            }
        }
    }
    let resumes_ok = resume_audio.count as u32;
    quiet();

    // CLOCK
    bounds::record_start(XA_MEASURE_RECORD + 12);
    clock = Watch::start();
    let drift = match long {
        Some(file) => drift_run(&mut clock, &mut progress, file),
        None => Drift::new(),
    };

    // Put the SPU back as it was, and the drive quiet.
    quiet();
    spu::set_cd_volume(CdVolume(cd_left as i16), CdVolume(cd_right as i16));
    spu::enable_cd_audio(spucnt & 1 != 0);

    let mut flags = bit(long.is_some(), 0)
        | bit(drift.refused, 1)
        | bit(drift.streamed, 2)
        | bit(drift.completed, 3);
    if songs.is_none() {
        flags |= 1 << 4;
    }
    [
        triple(0, start_audio.spread()),
        triple(1, start_head.spread()),
        triple(2, play_call.spread()),
        triple(3, by_filter.spread()),
        triple(4, filter_ack.spread()),
        triple(5, by_restart.spread()),
        triple(6, resume_audio.spread()),
        triple(7, restart_audio.spread()),
        triple(8, cdda_audio.spread()),
        triple(9, read_time.spread()),
        triple(10, moved.spread()),
        record(XA_MEASURE_RECORD + 11, starts_ok, switches_ok, resumes_ok),
        record(
            XA_MEASURE_RECORD + 12,
            drift.deviation[0],
            drift.deviation[1],
            drift.deviation[2],
        ),
        record(XA_MEASURE_RECORD + 13, flags, drift.seconds, drift.polls),
        record(
            XA_MEASURE_RECORD + 14,
            drift.distinct,
            drift.longest_stall,
            drift.line_clocks,
        ),
    ]
}

/// What the clock case saw.
struct Drift {
    refused: bool,
    streamed: bool,
    completed: bool,
    seconds: u32,
    polls: u32,
    deviation: [u32; 3],
    distinct: u32,
    longest_stall: u32,
    line_clocks: u32,
}

impl Drift {
    const fn new() -> Self {
        Self {
            refused: false,
            streamed: false,
            completed: false,
            seconds: 0,
            polls: 0,
            deviation: [0xFFFF; 3],
            distinct: 0,
            longest_stall: 0,
            line_clocks: 0,
        }
    }
}

/// Play the long song for three minutes, polling twice a second, and compare
/// the head's progress with the system clock.
#[inline(never)]
fn drift_run(clock: &mut Watch, progress: &mut Progress<'_>, file: File) -> Drift {
    let mut drift = Drift::new();
    quiet();
    // Quiet enough to leave on for three minutes.
    spu::set_cd_volume(CdVolume(0x0800), CdVolume(0x0800));
    let mut p = player();
    p.set_volume(0x20, 0x20);
    if p.play(file.song(LOW_CHANNEL), false).is_err() {
        drift.refused = true;
        p.release();
        return drift;
    }
    let mut frames = 0;
    while frames < STREAM_FRAMES && !p.is_streaming() {
        let _ = interrupts::try_wait_vblank(VBLANK_SPINS);
        p.poll();
        frames += 1;
    }
    if !p.is_streaming() {
        p.release();
        return drift;
    }
    drift.streamed = true;
    let start = clock.ticks();
    let lines_start = clock.last_lines;
    let head0 = p.elapsed_millis();
    let mut head = head0;
    let mut head_changed = start;
    let mut next_poll = POLL_TICKS;
    let mut next_mark = 0;
    let mut shown = 0;
    let end = DRIFT_SECONDS * SYS_TICKS_PER_SECOND;
    let mut elapsed = 0;
    // Bounded by iterations; a pass is a few dozen system clocks.
    for _ in 0..bounds::scale(DRIFT_ITERATIONS) {
        elapsed = clock.ticks().wrapping_sub(start);
        let due_mark = next_mark < DRIFT_MARKS.len()
            && elapsed >= DRIFT_MARKS[next_mark] * SYS_TICKS_PER_SECOND;
        if elapsed >= next_poll || due_mark {
            next_poll = elapsed + POLL_TICKS;
            p.poll();
            drift.polls += 1;
            let reading = p.elapsed_millis();
            if reading != head {
                head = reading;
                drift.distinct += 1;
                head_changed = clock.ticks();
            }
            let stood = sys_ms(clock.ticks().wrapping_sub(head_changed));
            drift.longest_stall = drift.longest_stall.max(stood);
            if due_mark {
                let progress_ms = reading as i32 - head0 as i32;
                drift.deviation[next_mark] = signed(progress_ms - sys_ms(elapsed) as i32);
                next_mark += 1;
            }
        }
        let seconds = elapsed / SYS_TICKS_PER_SECOND;
        drift.seconds = seconds;
        if seconds as usize > shown {
            shown = seconds as usize;
            progress.done = progress
                .done
                .max(Progress::TOTAL - DRIFT_SECONDS as usize + shown);
            progress.show("CLOCK DRIFT 3 MIN");
        }
        if elapsed >= end {
            drift.completed = next_mark == DRIFT_MARKS.len();
            break;
        }
    }
    // System clocks per HBlank line over the run, in hundredths, less 2000.
    let lines = clock.last_lines.wrapping_sub(lines_start);
    if lines > 0 && elapsed > 0 {
        let hundredths = elapsed / lines * 800 + elapsed % lines * 800 / lines;
        drift.line_clocks = hundredths.saturating_sub(200_000);
    }
    p.release();
    drift
}
