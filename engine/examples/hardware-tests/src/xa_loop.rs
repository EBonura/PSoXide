// SPDX-License-Identifier: GPL-2.0-or-later
//! CONSOLE TESTS: XA MUSIC LOOP. A short generated XA-ADPCM song looped
//! through the SDK's `psx_io::cd::xa::Player`, to measure on a console what
//! the emulator can only model.
//!
//! The player loops by asking the drive where its head is (GetlocP) once per
//! poll and restarting the song when the head passes the end, so this case
//! answers two questions at once. Does GetlocP keep updating while an XA
//! stream plays? If it does not, the song never loops and the screen says
//! so. And how long is the music interrupted at each loop? The gap shown is
//! the time from the player restarting the song (the head had reached its
//! end) to the first poll that finds the head back inside it, measured on
//! root counter 1 in HBlank units (about 64 microseconds), with a poll every
//! few milliseconds. The head runs a sector or two ahead of the sound, so the
//! audible gap is about that figure and no better.
//!
//! The song is `HWSONGS.XA` on the disc (see docs/hardware-test-disc.md): the
//! SDK's four generated tone songs as the channels of one 37.8 kHz stereo
//! file, single speed; this case plays channel 0.

use crate::console_tests::{
    put_number, record, spread, text, Buttons, Screen, XA_COUNT, XA_RECORD,
};
use crate::TimingRecord;
use core::ptr::addr_of_mut;
use psx_fmv::iso;
use psx_font::FontAtlas;
use psx_io::cd::xa::{DriveSpeed, Event, File, Player};
use psx_io::timers;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::interrupts;
use psx_spu::{self as spu, CdVolume, Volume};

/// The song file's name, and the numbers `xa-encode` printed in its manifest.
pub(crate) const SONG_FILE: &str = "HWSONGS.XA";
const FILE_NUMBER: u8 = 1;
const CHANNEL: u8 = 0;
const SECTORS_PER_SECOND: u32 = 75;

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

/// One data sector, for the directory lookup.
fn read_one(lba: u32) -> Option<&'static [u8]> {
    // SAFETY: single-threaded use of the reader and its sector buffer, with
    // nothing else driving the controller.
    unsafe {
        let reader = &mut *addr_of_mut!(READER);
        if !reader.start_read(lba) {
            return None;
        }
        let ok = reader.read_sector(&mut *addr_of_mut!(SECTOR));
        reader.stop();
        ok.then(|| core::slice::from_raw_parts(addr_of_mut!(SECTOR) as *const u8, SECTOR_WORDS * 4))
    }
}

/// The song file's place on the disc, looked up by name.
#[inline(never)]
fn find_song_file() -> Option<File> {
    // SAFETY: nothing else drives the controller yet.
    if !unsafe { (*addr_of_mut!(READER)).prepare() } {
        return None;
    }
    let (root, _) = iso::root_directory(read_one(iso::PVD_LBA)?)?;
    let (lba, size) = iso::find_in_directory(read_one(root)?, SONG_FILE)?;
    Some(File::from_directory_entry(
        lba,
        size,
        FILE_NUMBER,
        DriveSpeed::Single,
    ))
}

/// Root counter 1 counting HBlanks, unwrapped to 32 bits. Wraps every four
/// seconds, so it needs a read at least that often.
struct Clock {
    last: u16,
    high: u32,
}

/// HBlanks a second in the NTSC mode the suite runs in.
const HBLANKS_PER_SECOND: u32 = 15_734;

impl Clock {
    fn start() -> Self {
        timers::set_mode(timers::Timer::Timer1, 1 << 8);
        timers::set_counter(timers::Timer::Timer1, 0);
        Self { last: 0, high: 0 }
    }

    fn ticks(&mut self) -> u32 {
        let now = timers::counter(timers::Timer::Timer1);
        if now < self.last {
            self.high += 1;
        }
        self.last = now;
        self.high << 16 | now as u32
    }
}

fn millis(ticks: u32) -> u32 {
    ticks / HBLANKS_PER_SECOND * 1000 + ticks % HBLANKS_PER_SECOND * 1000 / HBLANKS_PER_SECOND
}

const KEPT: usize = 24;

/// What the run has seen.
#[derive(Copy, Clone)]
pub(crate) struct Run {
    pub(crate) found: bool,
    pub(crate) play_error: bool,
    pub(crate) streaming_seen: bool,
    pub(crate) no_loop: bool,
    pub(crate) loops: u32,
    pub(crate) gaps: [u32; KEPT],
    pub(crate) gap_count: usize,
    pub(crate) periods: [u32; KEPT],
    pub(crate) period_count: usize,
    /// Milliseconds from the first play to the head being seen in the song.
    pub(crate) first_start_ms: u32,
    /// Different head positions the polls reported, and the longest time one
    /// stayed unchanged while the song streamed.
    pub(crate) distinct: u32,
    pub(crate) max_stall_ms: u32,
    pub(crate) polls: u32,
    pub(crate) seconds: u32,
    pub(crate) length_ms: u32,
    pub(crate) elapsed_ms: u32,
    pub(crate) state: &'static str,
}

impl Run {
    const fn new() -> Self {
        Self {
            found: false,
            play_error: false,
            streaming_seen: false,
            no_loop: false,
            loops: 0,
            gaps: [0; KEPT],
            gap_count: 0,
            periods: [0; KEPT],
            period_count: 0,
            first_start_ms: 0,
            distinct: 0,
            max_stall_ms: 0,
            polls: 0,
            seconds: 0,
            length_ms: 0,
            elapsed_ms: 0,
            state: "STARTING",
        }
    }

    fn push(list: &mut [u32; KEPT], count: &mut usize, value: u32) {
        if *count < KEPT {
            list[*count] = value;
            *count += 1;
        } else {
            list.rotate_left(1);
            list[KEPT - 1] = value;
        }
    }

    /// Whether GetlocP looks alive: the position changed more than twice and
    /// never sat still for a second while streaming.
    pub(crate) fn getlocp_updates(&self) -> bool {
        self.distinct > 2 && self.max_stall_ms < 1000
    }
}

/// The result as records from `0x2E0`: 0x2E0 flags (bit 0 file found, 1 play
/// refused, 2 head seen in the song, 3 looped, 4 GetlocP updating, 5 no loop
/// by the song's length plus three seconds), loops, seconds run; 0x2E1 loop
/// gap ms min/median/max (0xFFFF if none); 0x2E2 loop period ms the same;
/// 0x2E3 first start ms, distinct head positions, longest unchanged ms.
pub(crate) fn records(run: &Run) -> [TimingRecord; XA_COUNT] {
    let flags = run.found as u32
        | (run.play_error as u32) << 1
        | (run.streaming_seen as u32) << 2
        | (run.loops > 0) as u32 * 8
        | (run.getlocp_updates() as u32) << 4
        | (run.no_loop as u32) << 5;
    let mut gaps = run.gaps;
    let mut periods = run.periods;
    let (g, p) = (
        spread(&mut gaps[..run.gap_count]),
        spread(&mut periods[..run.period_count]),
    );
    let none = |count: usize, s: (u32, u32, u32)| {
        if count == 0 {
            (0xFFFF, 0xFFFF, 0xFFFF)
        } else {
            s
        }
    };
    let (g, p) = (none(run.gap_count, g), none(run.period_count, p));
    [
        record(XA_RECORD, flags, run.loops, run.seconds),
        record(XA_RECORD + 1, g.0, g.1, g.2),
        record(XA_RECORD + 2, p.0, p.1, p.2),
        record(
            XA_RECORD + 3,
            run.first_start_ms,
            run.distinct,
            run.max_stall_ms,
        ),
    ]
}

const LABEL: (u8, u8, u8) = (150, 170, 200);
const VALUE: (u8, u8, u8) = (236, 240, 248);
const NOTE: (u8, u8, u8) = (255, 216, 96);

fn line(font: &FontAtlas, y: i16, label: &str, value: u32) {
    text(font, 8, y, label, LABEL);
    put_number(font, 120, y, value, VALUE);
}

#[inline(never)]
fn draw(font: &FontAtlas, run: &Run) {
    text(font, 8, 8, "XA MUSIC LOOP", VALUE);
    text(font, 120, 8, SONG_FILE, LABEL);
    text(font, 8, 22, "STATE", LABEL);
    let colour = if run.play_error || run.no_loop {
        (230, 100, 100)
    } else {
        NOTE
    };
    text(font, 120, 22, run.state, colour);
    line(font, 36, "SONG MS", run.length_ms);
    line(font, 48, "HEAD MS", run.elapsed_ms);
    line(font, 60, "LOOPS", run.loops);
    line(font, 72, "START MS", run.first_start_ms);
    let mut gaps = run.gaps;
    let (min, med, max) = spread(&mut gaps[..run.gap_count]);
    text(font, 8, 90, "RESTART GAP MS", LABEL);
    if run.gap_count != 0 {
        text(font, 8, 102, "MIN", LABEL);
        put_number(font, 40, 102, min, NOTE);
        text(font, 104, 102, "MED", LABEL);
        put_number(font, 136, 102, med, NOTE);
        text(font, 200, 102, "MAX", LABEL);
        put_number(font, 232, 102, max, NOTE);
        for (k, gap) in run.gaps[..run.gap_count].iter().rev().take(8).enumerate() {
            put_number(font, 8 + 38 * k as i16, 114, *gap, VALUE);
        }
    } else {
        text(font, 8, 102, "NO LOOP YET", LABEL);
    }
    let mut periods = run.periods;
    let (_, period, _) = spread(&mut periods[..run.period_count]);
    line(font, 132, "PERIOD MS", period);
    text(font, 8, 150, "GETLOCP", LABEL);
    let (words, colour) = if !run.streaming_seen {
        ("WAITING", LABEL)
    } else if run.getlocp_updates() {
        ("UPDATING", (96, 240, 128))
    } else {
        ("FROZEN OR SLOW", (230, 100, 100))
    };
    text(font, 120, 150, words, colour);
    line(font, 162, "POSITIONS", run.distinct);
    line(font, 174, "MAX STALL MS", run.max_stall_ms);
    line(font, 186, "POLLS", run.polls);
    text(font, 8, 218, "CROSS: BACK TO MENU", NOTE);
}

/// Frames between redraws: polling has to stay dense.
const REDRAW_EVERY: u32 = 3;

fn state_text(player: &Player, error: bool) -> &'static str {
    match (error, player.is_playing(), player.is_streaming()) {
        (true, _, _) => "PLAY REFUSED",
        (_, false, _) => "STOPPED",
        (_, true, false) => "SEEKING",
        (_, true, true) => "PLAYING",
    }
}

/// Play the song on loop until CROSS, START or TRIANGLE.
#[inline(never)]
pub(crate) fn run(screen: &mut Screen, font: &FontAtlas) -> Run {
    let mut run = Run::new();
    // SAFETY: plain SPU register reads; the caller restores them from this.
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
    let file = find_song_file();
    run.found = file.is_some();
    let mut buttons = Buttons::new();
    if let Some(file) = file {
        // SAFETY: nothing else drives the controller from here on; the
        // reader above has stopped, and the player is released at the end.
        let mut player = Player::new(unsafe { psx_io::periph::Cd::steal() });
        player.set_volume(0x80, 0x80);
        run.length_ms = file.span_sector_count() * 1000 / SECTORS_PER_SECOND;
        poll_loop(screen, font, &mut run, &mut player, file, &mut buttons);
        player.release();
    } else {
        run.state = "FILE NOT FOUND";
        while !buttons.poll().exit() {
            screen.clear((6, 8, 18));
            draw(font, &run);
            screen.present();
        }
    }
    psx_spu::set_cd_volume(
        psx_spu::CdVolume(cd_left as i16),
        psx_spu::CdVolume(cd_right as i16),
    );
    psx_spu::enable_cd_audio(spucnt & 1 != 0);
    run
}

#[inline(never)]
fn poll_loop(
    screen: &mut Screen,
    font: &FontAtlas,
    run: &mut Run,
    player: &mut Player,
    file: File,
    buttons: &mut Buttons,
) {
    let mut clock = Clock::start();
    let begun = clock.ticks();
    run.play_error = player.play(file.song(CHANNEL), true).is_err();
    let mut last_loop: Option<u32> = None;
    let mut awaiting_return = false;
    let mut head = u32::MAX;
    let mut head_changed = begun;
    let mut frame = interrupts::vblank_count();
    loop {
        let event = player.poll();
        let now = clock.ticks();
        run.polls += 1;
        run.elapsed_ms = player.elapsed_millis();
        if player.is_streaming() {
            if !run.streaming_seen {
                run.streaming_seen = true;
                run.first_start_ms = millis(now - begun);
            }
            if run.elapsed_ms != head {
                head = run.elapsed_ms;
                run.distinct += 1;
                head_changed = now;
            }
            run.max_stall_ms = run.max_stall_ms.max(millis(now - head_changed));
        }
        if event == Event::Looped {
            run.loops += 1;
            if let Some(before) = last_loop {
                Run::push(
                    &mut run.periods,
                    &mut run.period_count,
                    millis(now - before),
                );
            }
            last_loop = Some(now);
            awaiting_return = true;
            head_changed = now;
        } else if awaiting_return && player.is_streaming() {
            let gap = millis(now - last_loop.unwrap_or(now));
            Run::push(&mut run.gaps, &mut run.gap_count, gap);
            awaiting_return = false;
        }
        run.seconds = millis(now - begun) / 1000;
        run.no_loop = run.loops == 0 && millis(now - begun) > run.length_ms + 3000;
        run.state = if run.no_loop {
            "NO LOOP SEEN"
        } else {
            state_text(player, run.play_error)
        };
        if buttons.poll().exit() {
            break;
        }
        let v = interrupts::vblank_count();
        if v.wrapping_sub(frame) >= REDRAW_EVERY {
            frame = v;
            screen.clear((6, 8, 18));
            draw(font, run);
            screen.flip();
        }
    }
}
