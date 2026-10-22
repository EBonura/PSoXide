//! `hello-xa-resume` -- a song survives a data read.
//!
//! Plays one channel of `SONGS.XA`, suspends it, reads sixteen data sectors
//! through the polled reader (which rewrites the drive mode and leaves the head
//! at the data), resumes, and checks what the player and the song clock saw:
//!
//! 1. the song streams, and the song clock stays with the head readings;
//! 2. the data read returns every sector it asked for;
//! 3. `resume` is accepted;
//! 4. the head is back inside the song file (not at the data) within five
//!    seconds, near where the pause left it, and keeps pace afterwards.
//!
//! The channel and the mode are checked outside, from the drive command log and
//! the audio capture (`tools/xa_resume_gate.sh`). The verdict goes to the TTY
//! at the end (`hello-xa-resume: ALL PASS`) and to the screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_fmv::iso;
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::cd::xa::{DriveSpeed, File, Player};
use psx_io::cd::xa_clock::SongClock;
use psx_io::periph::Cd;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::interrupts::{vblank_count, wait_vblank};
use psx_spu::{self as spu, CdVolume, Spu, Volume};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const SONGS_FILE: &str = "SONGS.XA";
const FILE_NUMBER: u8 = 1;
/// The whistle: a continuous 950 to 1900 Hz sweep, easy to tell from the
/// default filter (file 1, channel 1) a drive starts with.
const CHANNEL: u8 = 3;
const VBLANK_HZ: u32 = 60;
/// Frames the song plays before the data read, and after the resume.
const PLAY_FRAMES: u32 = 4 * VBLANK_HZ;
/// Frames to wait for the head to reach the song, at the start and after the
/// resume.
const PATIENCE_FRAMES: u32 = 5 * VBLANK_HZ;
/// Data sectors read while the song is suspended, from the ISO volume
/// descriptors on.
const DATA_SECTORS: u32 = 16;
const DATA_LBA: u32 = iso::PVD_LBA;

/// After the resume the player is polled this often, as a game that only
/// needs the clock would (the clock interpolates in between).
const SPARSE_POLL_FRAMES: u32 = 20;
/// How far the clock may sit from a fresh head reading: half a sector, a
/// frame, and the emulator's frame period against the nominal 60 Hz.
const CLOCK_SLACK_MS: u32 = 40;

const MAX_LINES: usize = 12;

struct Report {
    text: [u8; 1024],
    len: usize,
    ok: bool,
    lines: [(u16, u16, bool); MAX_LINES],
    line_count: usize,
}

static mut REPORT: Report = Report {
    text: [0; 1024],
    len: 0,
    ok: true,
    lines: [(0, 0, false); MAX_LINES],
    line_count: 0,
};

impl Report {
    fn push(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len < self.text.len() {
                self.text[self.len] = b;
                self.len += 1;
            }
        }
    }

    fn num(&mut self, mut value: u32) {
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        for &b in &digits[at..] {
            if self.len < self.text.len() {
                self.text[self.len] = b;
                self.len += 1;
            }
        }
    }

    /// One check: `name`, its verdict and the numbers behind it.
    fn check(&mut self, name: &str, ok: bool, values: &[(&str, u32)]) {
        let from = self.len;
        self.push("hello-xa-resume: ");
        self.push(name);
        self.push(if ok { ": ok" } else { ": FAIL" });
        for &(label, value) in values {
            self.push(" ");
            self.push(label);
            self.push("=");
            self.num(value);
        }
        self.push("\n");
        if self.line_count < MAX_LINES {
            self.lines[self.line_count] = (from as u16, self.len as u16, ok);
            self.line_count += 1;
        }
        self.ok &= ok;
    }

    fn text(&self) -> &str {
        core::str::from_utf8(&self.text[..self.len]).unwrap_or("?")
    }
}

fn read_one<'s>(
    reader: &mut SectorReader,
    sector: &'s mut [u32; SECTOR_WORDS],
    lba: u32,
) -> Option<&'s [u8]> {
    if !reader.start_read(lba) {
        return None;
    }
    let ok = reader.read_sector(sector);
    reader.stop();
    // SAFETY: a `[u32; N]` viewed as its own bytes; alignment only shrinks.
    ok.then(|| unsafe {
        core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), SECTOR_WORDS * 4)
    })
}

fn find_songs(cd: Cd) -> (Option<File>, Cd) {
    let mut reader = SectorReader::with_cd(cd);
    let mut sector = [0u32; SECTOR_WORDS];
    let file = lookup(&mut reader, &mut sector);
    (file, reader.release())
}

fn lookup(reader: &mut SectorReader, sector: &mut [u32; SECTOR_WORDS]) -> Option<File> {
    if !reader.prepare() {
        return None;
    }
    let (root, _) = iso::root_directory(read_one(reader, sector, iso::PVD_LBA)?)?;
    let (lba, size) = iso::find_in_directory(read_one(reader, sector, root)?, SONGS_FILE)?;
    Some(File::from_directory_entry(
        lba,
        size,
        FILE_NUMBER,
        DriveSpeed::Single,
    ))
}

/// What one run of the song looked like to the player and the clock.
#[derive(Copy, Clone)]
struct Run {
    /// Frames until the head was inside the file, or `None` if it never was.
    frames_to_stream: Option<u32>,
    /// `Player::elapsed_millis` at the first streaming frame.
    first_elapsed: u32,
    /// Clock reading at the first streaming frame, and at the end.
    clock_first: u32,
    clock_last: u32,
    /// Frames from the first streaming frame to the end.
    streamed_frames: u32,
    /// `Player::elapsed_millis` at the end.
    last_elapsed: u32,
    /// The clock never read lower than before.
    monotonic: bool,
    /// Largest distance between the clock and a fresh head reading (the
    /// middle of its sector), in milliseconds, at the frames that took one.
    worst_clock_error: u32,
}

/// Update the clock every frame, and poll the player every `poll_every`
/// frames, until the head has been inside the file for `play_frames` frames or
/// `PATIENCE_FRAMES` pass without it.
fn run_song(player: &mut Player, clock: &mut SongClock, play_frames: u32, poll_every: u32) -> Run {
    let mut run = Run {
        frames_to_stream: None,
        first_elapsed: 0,
        clock_first: 0,
        clock_last: 0,
        streamed_frames: 0,
        last_elapsed: 0,
        monotonic: true,
        worst_clock_error: 0,
    };
    let mut last_serial = None;
    let begin = vblank_count();
    let mut streaming_since = 0;
    loop {
        wait_vblank();
        let now = vblank_count();
        if now.wrapping_sub(begin) % poll_every == 0 {
            player.poll();
        }
        let reading = clock.update(player, now);
        if let Some(ms) = reading {
            if run.frames_to_stream.is_none() {
                run.frames_to_stream = Some(now.wrapping_sub(begin));
                run.first_elapsed = player.elapsed_millis();
                run.clock_first = ms;
                streaming_since = now;
            } else if ms < run.clock_last {
                run.monotonic = false;
            }
            if let Some(sample) = player.head_sample() {
                if last_serial != Some(sample.serial) {
                    last_serial = Some(sample.serial);
                    run.worst_clock_error = run
                        .worst_clock_error
                        .max(ms.abs_diff(sample.centre_millis()));
                }
            }
            run.clock_last = ms;
            run.streamed_frames = now.wrapping_sub(streaming_since);
            run.last_elapsed = player.elapsed_millis();
            if run.streamed_frames >= play_frames {
                return run;
            }
        } else if run.frames_to_stream.is_none() && now.wrapping_sub(begin) > PATIENCE_FRAMES {
            return run;
        }
    }
}

fn within(value: u32, expected: u32, slack: u32) -> bool {
    value.abs_diff(expected) <= slack
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    let _spu = Spu::new(peripherals.spu_dma);
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);

    // SAFETY: the only reference to the report; the program is single-threaded
    // and interrupts never touch it.
    let report = unsafe { &mut *core::ptr::addr_of_mut!(REPORT) };
    let (file, cd) = find_songs(peripherals.cd);
    let mut player = Player::new(cd);
    let mut clock = SongClock::new(VBLANK_HZ);

    if let Some(file) = file {
        let started = player.play(file.song(CHANNEL), false).is_ok();
        let before = run_song(&mut player, &mut clock, PLAY_FRAMES, 1);
        let streamed = started && before.frames_to_stream.is_some();
        report.check(
            "song streams",
            streamed,
            &[
                ("frames", before.frames_to_stream.unwrap_or(0)),
                ("at_ms", before.first_elapsed),
            ],
        );
        // The clock sits on the head readings (half a sector and a frame of
        // slack), and never moved backwards.
        report.check(
            "clock follows the head",
            streamed && before.monotonic && before.worst_clock_error <= CLOCK_SLACK_MS,
            &[("worst_ms", before.worst_clock_error)],
        );

        // The pause position, as the player last saw it.
        let paused_at = player.elapsed_millis();
        let read = player.with_cd_suspended(|cd| {
            let mut reader = SectorReader::with_cd(cd);
            let mut sector = [0u32; SECTOR_WORDS];
            let mut got = 0;
            if reader.prepare() && reader.start_read(DATA_LBA) {
                while got < DATA_SECTORS && reader.read_sector(&mut sector) {
                    got += 1;
                }
                reader.stop();
            }
            (reader.release(), got)
        });
        let sectors = read.unwrap_or(0);
        report.check(
            "data read while suspended",
            read.is_ok() && sectors == DATA_SECTORS,
            &[("sectors", sectors)],
        );

        let resumed = player.resume();
        report.check("resume accepted", resumed.is_ok(), &[]);

        let after = run_song(&mut player, &mut clock, PLAY_FRAMES, SPARSE_POLL_FRAMES);
        let back = resumed.is_ok() && after.frames_to_stream.is_some();
        report.check(
            "head back in the song",
            back,
            &[
                ("frames", after.frames_to_stream.unwrap_or(0)),
                ("paused_ms", paused_at),
                ("resumed_ms", after.first_elapsed),
            ],
        );
        // Near the pause: not at the top of the file, not far ahead. The
        // window covers the seek back from the data and a sector or two.
        report.check(
            "position kept",
            back && after.first_elapsed + 100 >= paused_at
                && after.first_elapsed <= paused_at + 700,
            &[
                ("paused_ms", paused_at),
                ("resumed_ms", after.first_elapsed),
            ],
        );
        let expected_ms = after.streamed_frames * 1000 / VBLANK_HZ;
        let advanced = after.last_elapsed.saturating_sub(after.first_elapsed);
        report.check(
            "keeps playing",
            back && after.monotonic && within(advanced, expected_ms, 100 + SPARSE_POLL_FRAMES * 17),
            &[("advanced_ms", advanced), ("frames_ms", expected_ms)],
        );
        // Polling every 20 frames, as a game would, the clock is smooth in
        // between and starts again from the resumed position.
        report.check(
            "clock restarts at the resumed position",
            back && after.monotonic
                && within(after.clock_first, after.first_elapsed, 60)
                && after.worst_clock_error <= CLOCK_SLACK_MS,
            &[
                ("clock_first_ms", after.clock_first),
                ("elapsed_first_ms", after.first_elapsed),
                ("worst_ms", after.worst_clock_error),
            ],
        );
        player.stop();
    } else {
        report.check("song file found", false, &[]);
    }

    psx_rt::tty::print(report.text());
    psx_rt::tty::println(if report.ok {
        "hello-xa-resume: ALL PASS"
    } else {
        "hello-xa-resume: FAIL"
    });

    loop {
        let bg = if report.ok {
            (16, 96, 32)
        } else {
            (110, 20, 20)
        };
        fb.clear(&mut gpu, bg);
        font.draw_text(8, 6, "HELLO-XA-RESUME", (230, 230, 240));
        let mut y: i16 = 22;
        for i in 0..report.line_count {
            let (from, to, ok) = report.lines[i];
            let text =
                core::str::from_utf8(&report.text[from as usize..to as usize]).unwrap_or("?");
            let name = text
                .strip_prefix("hello-xa-resume: ")
                .and_then(|t| t.split(':').next())
                .unwrap_or("?");
            let tint = if ok { (80, 220, 100) } else { (230, 80, 80) };
            font.draw_text(8, y, if ok { "OK" } else { "XX" }, tint);
            font.draw_text(32, y, name, tint);
            y += 12;
        }
        gpu.wait_idle();
        wait_vblank();
        fb.swap(&mut gpu);
    }
}
