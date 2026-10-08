// SPDX-License-Identifier: GPL-2.0-or-later
//! v1.28 CD STREAM cases (CONSOLE TESTS): what `psx-cdstream`, the SDK's
//! interrupt-driven CD transport, costs and does on a console. They answer
//! the measurement list of the streaming design (docs/streaming-design-
//! 2026-10-08.md, milestone M0) that the characterisation capture cannot,
//! because that capture reads by polling and never runs the transport.
//!
//! - `CD STREAM COST`: a sustained read through the transport at double and
//!   at single speed. Sectors per second, the CPU the sector pops take from a
//!   foreground loop (microseconds per sector and the share of the CPU lost),
//!   the longest interrupt handler call (Timer 2), interrupts per sector and
//!   the time to the first sector of a cold read.
//! - `CD-DA HANDOFF`: the audio lease. How long a lease request takes to stop
//!   a read, how long a Pause takes to leave the controller idle after CD-DA
//!   (`T_pause`), the first data sector after audio, and whether audio
//!   resumed at the position GetlocP saved. Then three data reads that start
//!   while the tone is playing (after a proper Pause; with the transport's
//!   recovery Pause; with no Pause at all), each with objective evidence of
//!   whether CD-DA lived through it: the drive's PLAYING bit, GetlocP
//!   advancing, and the SPU capture buffer for CD input sampled while the
//!   read runs. No listening is needed.
//! - `CD MOTOR`: a read after a Pause and a wait of 0, 5 and 15 seconds; a
//!   read right after Stop; a read after Stop once the motor has stopped.
//!
//! Each case leaves timing-block records (`0x2F0`, `0x300`, `0x310`) in the
//! capture; the layouts are in each function's doc and in
//! tools/hwtest-report.py. They install and remove the transport themselves,
//! so a case is safe to run after the others; run them after the full
//! characterisation, which they would otherwise disturb (the transport's
//! exception wrapper stays in the vector after it is removed).

use crate::console_tests::{
    number, record, spread, text, Buttons, ConsoleCase, Screen, CDCOST_COUNT, CDCOST_RECORD,
    CDHANDOFF_COUNT, CDHANDOFF_RECORD, CDMOTOR_COUNT, CDMOTOR_RECORD,
};
use crate::TimingRecord;
use core::hint::black_box;
use core::ptr::addr_of_mut;
use psx_cdstream::{
    Completion, Config, LeaseState, Outcome, Priority, Request, RequestState, Ticket,
};
use psx_font::FontAtlas;
use psx_hw::cd::irq as flag;
use psx_hw::cd::{CMD_PAUSE, CMD_PLAY, CMD_SETLOC, CMD_STOP, MODE_AUTO_PAUSE, MODE_CDDA};
use psx_io::cd::{bin_to_bcd, PlayPosition};
use psx_io::periph::Cd;
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;
use psx_spu::{self as spu, CdVolume, Volume};

/// The deterministic read region the disc build puts on every hardware-test
/// disc: `CDTEST.BIN`, 460 sectors of the `PSOXSTRM` pattern ending where the
/// packs would start (LBA 1024). Every sector is checked against the pattern,
/// so a layout that moved reads as failed data rather than as a measurement.
const FILE_LBA: u32 = 564;
const FILE_SECTORS: u32 = 460;
const SECTOR_WORDS: usize = 512;
/// Sectors in each request of a stream, and requests kept in flight.
const PER: u32 = 4;
const SLOTS: usize = 4;
/// First CD-DA track after the data track: the synthetic tone.
const TONE_TRACK: u8 = 2;
const CDDA_MODE: u8 = MODE_CDDA | MODE_AUTO_PAUSE;
/// HBlanks a second in the NTSC mode the cases run in.
const HZ: u32 = 15_734;
/// Longest wait for any one drive operation, in HBlanks (10 s).
const PATIENCE: u32 = 10 * HZ;
const CODE_TIMEOUT: u32 = 0xFFFF_FFFF;
const CODE_CANCELLED: u32 = 0xFFFF_FFFE;
/// Drive status byte bits.
const STAT_MOTOR_ON: u8 = 0x02;
const STAT_PLAYING: u8 = 0x80;
/// Peak-to-peak of the SPU's CD-input capture buffer that counts as audio.
/// The tone is half scale; CD volume is set to half again.
const SIGNAL: u32 = 1000;
/// Words of the CD-left capture bank (SPU RAM 0 to 0x3FF).
const CAPTURE_WORDS: usize = 256;

static mut RING: [[u32; PER as usize * SECTOR_WORDS]; SLOTS] =
    [[0; PER as usize * SECTOR_WORDS]; SLOTS];

// -------------------------------------------------------------------- clock

static mut CLOCK_LAST: u16 = 0;
static mut CLOCK_HIGH: u32 = 0;

/// Timer 1 counting HBlanks, widened to 32 bits by noticing wraps. Every
/// wait loop reads it far more often than the four seconds a wrap takes.
fn clock_start() {
    timers::set_mode(Timer::Timer1, 1 << 8);
    timers::set_counter(Timer::Timer1, 0);
    // SAFETY: single thread of control; only `now` touches these.
    unsafe {
        CLOCK_LAST = 0;
        CLOCK_HIGH = 0;
    }
}

#[inline(never)]
fn now() -> u32 {
    let c = timers::counter(Timer::Timer1);
    // SAFETY: as `clock_start`.
    unsafe {
        if c < CLOCK_LAST {
            CLOCK_HIGH += 1;
        }
        CLOCK_LAST = c;
        (CLOCK_HIGH << 16) | u32::from(c)
    }
}

#[inline(never)]
fn since(start: u32) -> u32 {
    now().wrapping_sub(start)
}

/// Milliseconds in tenths: `hblanks * 63.556 us`, rounded.
#[inline(never)]
fn ms10(hblanks: u32) -> u32 {
    (hblanks.saturating_mul(6356) + 5000) / 10_000
}

fn ms(hblanks: u32) -> u32 {
    (ms10(hblanks) + 5) / 10
}

/// Microseconds of a Timer 2 count (system clock / 8).
#[inline(never)]
fn timer2_us(ticks: u32) -> u32 {
    ticks.saturating_mul(10_000) / 42_336
}

#[inline(never)]
fn delay(hblanks: u32) {
    let start = now();
    while since(start) < hblanks {
        psx_cdstream::service();
    }
}

// --------------------------------------------------------------------- text

/// A line of text built in place.
#[derive(Copy, Clone)]
struct Line {
    buf: [u8; 38],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            buf: [0; 38],
            len: 0,
        }
    }

    #[inline(never)]
    fn s(&mut self, t: &str) -> &mut Self {
        for b in t.bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }

    #[inline(never)]
    fn u(&mut self, v: u32) -> &mut Self {
        let mut digits = [0u8; 10];
        self.s(number(&mut digits, v))
    }

    #[inline(never)]
    fn yn(&mut self, v: bool) -> &mut Self {
        self.s(if v { "YES" } else { "NO" })
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

const VALUE: (u8, u8, u8) = (236, 240, 248);
const LABEL: (u8, u8, u8) = (150, 170, 200);
const NOTE: (u8, u8, u8) = (255, 216, 96);
const BAD: (u8, u8, u8) = (230, 100, 100);

struct Ui<'a, 'g> {
    screen: &'a mut Screen<'g>,
    font: &'a FontAtlas,
}

impl Ui<'_, '_> {
    /// A progress screen: what the case is doing now.
    #[inline(never)]
    fn progress(&mut self, title: &str, a: &str, b: &str) {
        self.screen.clear((6, 8, 18));
        text(self.font, 8, 8, title, VALUE);
        text(self.font, 8, 36, a, NOTE);
        text(self.font, 8, 50, b, LABEL);
        self.screen.flip();
    }

    /// The result page, up until CROSS: one row per record (its three fields
    /// as min, median, max), then any extra lines.
    #[inline(never)]
    fn table(
        &mut self,
        title: &str,
        ok: bool,
        names: &[&str],
        records: &[TimingRecord],
        extra: &[Line],
    ) {
        let mut rows = [Line::new(); 16];
        let mut n = 0;
        for (name, record) in names.iter().zip(records) {
            rows[n]
                .s(name)
                .s(" ")
                .u(u32::from(record.min))
                .s(" ")
                .u(u32::from(record.med))
                .s(" ")
                .u(u32::from(record.max));
            n += 1;
        }
        for line in extra.iter().take(rows.len() - n) {
            rows[n] = *line;
            n += 1;
        }
        let mut buttons = Buttons::new();
        loop {
            self.screen.clear((6, 8, 18));
            text(self.font, 8, 6, title, VALUE);
            text(
                self.font,
                232,
                6,
                if ok { "OK" } else { "CHECK" },
                if ok { NOTE } else { BAD },
            );
            for (i, line) in rows[..n].iter().enumerate() {
                text(
                    self.font,
                    8,
                    20 + 11 * i as i16,
                    line.as_str(),
                    if i < records.len() { LABEL } else { NOTE },
                );
            }
            text(self.font, 8, 224, "CROSS: BACK TO MENU", NOTE);
            self.screen.present();
            if buttons.poll().exit() {
                break;
            }
        }
    }
}

// ------------------------------------------------------------------ session

/// The case's hold on the disc, the SPU and the transport.
struct Session {
    spucnt: u16,
    cd_left: u16,
    cd_right: u16,
}

impl Session {
    /// Set the SPU up for the tone, start the clock and install the
    /// transport.
    #[inline(never)]
    fn open(ui: &mut Ui, title: &str) -> Session {
        ui.progress(title, "STARTING THE TRANSPORT", "");
        // SAFETY: plain SPU register reads; `close` restores them.
        let (spucnt, cd_left, cd_right) = unsafe {
            (
                psx_io::read_u16(psx_hw::spu::SPUCNT),
                psx_io::read_u16(0x1F80_1DB0),
                psx_io::read_u16(0x1F80_1DB2),
            )
        };
        spu::init();
        spu::set_main_volume(Volume::MAX, Volume::MAX);
        spu::set_cd_volume(CdVolume(0x4000), CdVolume(0x4000));
        spu::enable_cd_audio(true);
        clock_start();
        // SAFETY: the polled reader above has stopped; nothing else drives
        // the controller from here until `close`.
        let cd = unsafe { Cd::steal() };
        if let Err(_already) = psx_cdstream::install(cd, config(true, true)) {
            psx_cdstream::configure(config(true, true));
        }
        Session {
            spucnt,
            cd_left,
            cd_right,
        }
    }

    /// Stop whatever the drive is doing, take the transport out and put the
    /// SPU back.
    #[inline(never)]
    fn close(self) {
        psx_cdstream::cancel_all();
        wait_idle();
        quiet();
        let _ = psx_cdstream::uninstall();
        spu::set_cd_volume(
            CdVolume(self.cd_left as i16),
            CdVolume(self.cd_right as i16),
        );
        spu::enable_cd_audio(self.spucnt & 1 != 0);
    }
}

fn config(double_speed: bool, pause_after_audio: bool) -> Config {
    Config {
        timeout_vblanks: 240,
        time_handler: true,
        double_speed,
        pause_after_audio,
        ..Config::DEFAULT
    }
}

#[inline(never)]
fn ring(slot: usize) -> *mut u32 {
    // SAFETY: `slot` < SLOTS; the address is formed without a reference.
    unsafe {
        addr_of_mut!(RING)
            .cast::<[u32; PER as usize * SECTOR_WORDS]>()
            .add(slot)
            .cast()
    }
}

// ------------------------------------------------------------------ pattern

/// Byte `index` of the read region: the same pattern `psx_iso` writes.
#[inline(never)]
fn expected_byte(index: u32, sectors: u32) -> u8 {
    const MAGIC: &[u8; 8] = b"PSOXSTRM";
    if index < 8 {
        MAGIC[index as usize]
    } else if index < 12 {
        sectors.to_le_bytes()[(index - 8) as usize]
    } else {
        index
            .wrapping_mul(37)
            .wrapping_add(index >> 3)
            .wrapping_add(0x5D) as u8
    }
}

#[inline(never)]
fn expected_word(word: u32, sectors: u32) -> u32 {
    let at = word * 4;
    u32::from(expected_byte(at, sectors))
        | u32::from(expected_byte(at + 1, sectors)) << 8
        | u32::from(expected_byte(at + 2, sectors)) << 16
        | u32::from(expected_byte(at + 3, sectors)) << 24
}

/// Do the `sectors` sectors at `buffer`, which hold file sectors
/// `file_sector..`, carry the pattern? `stride` checks one word in that many.
#[inline(never)]
fn verify(buffer: *const u32, file_sector: u32, sectors: u32, stride: u32) -> bool {
    let total = FILE_SECTORS;
    let first_word = file_sector * SECTOR_WORDS as u32;
    let mut word = 0;
    while word < sectors * SECTOR_WORDS as u32 {
        // SAFETY: inside the sectors the caller owns; the request that
        // filled them has finished.
        let got = unsafe { buffer.add(word as usize).read_volatile() };
        if got != expected_word(first_word + word, total) {
            return false;
        }
        word += stride;
    }
    true
}

// -------------------------------------------------------------------- reads

#[inline(never)]
fn submit(file_sector: u32, sectors: u32, destination: *mut u32) -> Option<Ticket> {
    // SAFETY: every destination is a ring slot of at least `sectors` sectors
    // that nothing touches until the request has finished.
    let request = unsafe { Request::new_raw(FILE_LBA + file_sector, sectors, destination) };
    psx_cdstream::submit(request.with_priority(Priority::NORMAL)).ok()
}

#[inline(never)]
fn wait(ticket: Ticket) -> Option<Completion> {
    let start = now();
    loop {
        if let RequestState::Finished(done) = psx_cdstream::state(ticket) {
            return Some(done);
        }
        if since(start) > PATIENCE {
            return None;
        }
    }
}

/// Wait until the drive is stopped and nothing is queued.
#[inline(never)]
fn wait_idle() {
    let start = now();
    while !(psx_cdstream::is_idle() && psx_cdstream::queued_count() == 0) {
        if since(start) > PATIENCE {
            return;
        }
    }
}

/// 0 when the request was done in full, else why not.
#[inline(never)]
fn code_of(done: Option<Completion>, sectors: u32) -> u32 {
    match done {
        Some(Completion {
            outcome: Outcome::Done,
            received,
            ..
        }) if received == sectors => 0,
        Some(Completion {
            outcome: Outcome::Failed(failure),
            ..
        }) => failure.code().max(1),
        Some(Completion {
            outcome: Outcome::Cancelled,
            ..
        }) => CODE_CANCELLED,
        _ => CODE_TIMEOUT,
    }
}

/// A failure code as a record field.
#[inline(never)]
fn code16(code: u32) -> u32 {
    match code {
        CODE_TIMEOUT => 0xFFFF,
        CODE_CANCELLED => 0xFFFE,
        other => other.min(0xFFFD),
    }
}

/// What one stream did, in HBlanks from the first submit.
#[derive(Copy, Clone)]
struct Streamed {
    ok: bool,
    code: u32,
    /// Until the first sector had landed (0 if none did).
    first: u32,
    /// Until the last sector had landed.
    done: u32,
    /// Foreground loop iterations.
    iterations: u32,
}

/// Read `total` sectors from `start` of the region as a chain of requests of
/// `PER` sectors kept `SLOTS` deep, calling `hook` with the HBlanks elapsed
/// on every pass of the foreground loop. Sampled verification (`stride`).
#[inline(never)]
fn stream(start: u32, total: u32, stride: u32, hook: &mut dyn FnMut(u32)) -> Streamed {
    let mut out = Streamed {
        ok: false,
        code: CODE_TIMEOUT,
        first: 0,
        done: 0,
        iterations: 0,
    };
    let mut tickets: [Option<Ticket>; SLOTS] = [None; SLOTS];
    let mut lens = [0u32; SLOTS];
    let mut firsts = [0u32; SLOTS];
    let mut next = 0u32;
    let t0 = now();
    for slot in 0..SLOTS {
        if next < total {
            let n = PER.min(total - next);
            tickets[slot] = submit(start + next, n, ring(slot));
            lens[slot] = n;
            firsts[slot] = start + next;
            next += n;
        }
    }
    let mut landed = 0u32;
    let mut slot = 0;
    loop {
        let elapsed = since(t0);
        out.iterations += 1;
        hook(elapsed);
        let Some(ticket) = tickets[slot] else {
            out.code = if landed == total { 0 } else { CODE_TIMEOUT };
            out.ok = landed == total;
            break;
        };
        match psx_cdstream::state(ticket) {
            RequestState::Finished(done) => {
                let code = code_of(Some(done), lens[slot]);
                if out.first == 0 {
                    out.first = since(t0);
                }
                if code != 0 {
                    out.code = code;
                    break;
                }
                if !verify(ring(slot), firsts[slot], lens[slot], stride) {
                    out.code = 0x00DA;
                    break;
                }
                landed += lens[slot];
                out.done = since(t0);
                if next < total {
                    let n = PER.min(total - next);
                    tickets[slot] = submit(start + next, n, ring(slot));
                    lens[slot] = n;
                    firsts[slot] = start + next;
                    next += n;
                } else {
                    tickets[slot] = None;
                }
                slot = (slot + 1) % SLOTS;
            }
            RequestState::Active { received } if out.first == 0 && received >= 1 => {
                out.first = since(t0);
            }
            _ => {}
        }
        if elapsed > PATIENCE {
            out.code = CODE_TIMEOUT;
            break;
        }
    }
    if !out.ok {
        psx_cdstream::cancel_all();
    }
    wait_idle();
    out
}

/// One small read, timed, for the cases that only need the first sector.
#[inline(never)]
fn read_small(start: u32, sectors: u32) -> Streamed {
    stream(start, sectors, 1, &mut |_| {})
}

// -------------------------------------------------------------------- audio

/// Ask for the drive on behalf of audio and collect the controller token.
#[inline(never)]
fn acquire_audio() -> Option<Cd> {
    wait_idle();
    let _ = psx_cdstream::request_audio_lease();
    let start = now();
    while psx_cdstream::lease_state() != LeaseState::Granted {
        if since(start) > PATIENCE {
            let _ = psx_cdstream::withdraw_audio_lease();
            return None;
        }
    }
    psx_cdstream::take_audio_lease()
}

#[inline(never)]
fn release_audio(cd: Cd) {
    let _ = psx_cdstream::release_audio_lease(cd);
}

#[inline(never)]
fn stat(cd: &mut Cd) -> u8 {
    cd.status()
        .ok()
        .and_then(|r| r.bytes().first().copied())
        .unwrap_or(0xFF)
}

#[inline(never)]
fn play_position(cd: &mut Cd) -> Option<PlayPosition> {
    cd.play_position()
        .ok()
        .and_then(|response| PlayPosition::parse(&response))
}

#[inline(never)]
fn absolute_frames(p: &PlayPosition) -> i32 {
    ((i32::from(p.absolute_min) * 60 + i32::from(p.absolute_sec)) * 75)
        + i32::from(p.absolute_frame)
}

/// A Pause with the transport out of the way, so no audio or read is left.
#[inline(never)]
fn quiet() {
    if let Some(mut cd) = acquire_audio() {
        let _ = timed_command(&mut cd, CMD_PAUSE, &[], true, 3 * HZ);
        release_audio(cd);
    }
}

/// When a polled command's responses arrived, in HBlanks from the command.
#[derive(Copy, Clone)]
struct CommandTiming {
    ack: u32,
    complete: u32,
}

/// Send a command and time its acknowledge and completion by polling the
/// controller's flag register. `None` if the parameter FIFO never had room
/// or no acknowledge came inside `limit` HBlanks.
#[inline(never)]
fn timed_command(
    cd: &mut Cd,
    command: u8,
    params: &[u8],
    wait_complete: bool,
    limit: u32,
) -> Option<CommandTiming> {
    cd.set_irq_enable_mask(0);
    cd.acknowledge_all_and_reset_parameters();
    cd.discard_response();
    cd.reset_parameter_fifo();
    for &param in params {
        if !cd.wait_parameter_room(4096) {
            return None;
        }
        cd.send_parameter_byte(param);
    }
    let t0 = now();
    cd.send_command_byte(command);
    let mut timing = CommandTiming {
        ack: 0,
        complete: 0,
    };
    loop {
        match cd.irq_flag_value() {
            flag::ACKNOWLEDGE => {
                timing.ack = since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::ACKNOWLEDGE);
                break;
            }
            flag::ERROR => {
                timing.ack = since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::ACK_ALL);
                return Some(timing);
            }
            _ => {}
        }
        if since(t0) > limit {
            cd.acknowledge_irq(flag::ACK_ALL);
            return None;
        }
    }
    if !wait_complete {
        return Some(timing);
    }
    loop {
        match cd.irq_flag_value() {
            flag::COMPLETE => {
                timing.complete = since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::COMPLETE);
                break;
            }
            flag::ERROR => {
                timing.complete = since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::ACK_ALL);
                break;
            }
            _ => {}
        }
        if since(t0) > limit {
            cd.acknowledge_irq(flag::ACK_ALL);
            break;
        }
    }
    Some(timing)
}

/// Start the tone from the top. (HBlanks until the drive reports PLAYING,
/// whether it ever did.)
#[inline(never)]
fn start_tone(cd: &mut Cd) -> (u32, bool) {
    let _ = cd.set_mode(CDDA_MODE);
    let _ = cd.unmute();
    let t0 = now();
    let _ = cd.play_track(TONE_TRACK);
    while since(t0) < 4 * HZ {
        if stat(cd) & STAT_PLAYING != 0 {
            return (since(t0), true);
        }
    }
    (since(t0), false)
}

/// Resume at an absolute position: (HBlanks until PLAYING, whether it did).
#[inline(never)]
fn resume_at(cd: &mut Cd, at: &PlayPosition) -> (u32, bool) {
    let _ = cd.set_mode(CDDA_MODE);
    let _ = cd.unmute();
    let target = [
        bin_to_bcd(at.absolute_min),
        bin_to_bcd(at.absolute_sec),
        bin_to_bcd(at.absolute_frame),
    ];
    let _ = cd.command(CMD_SETLOC, &target);
    let t0 = now();
    let _ = cd.command(CMD_PLAY, &[]);
    while since(t0) < 4 * HZ {
        if stat(cd) & STAT_PLAYING != 0 {
            return (since(t0), true);
        }
    }
    (since(t0), false)
}

/// Peak-to-peak of the SPU's CD-left capture buffer: the last 11 ms of CD
/// input as it left the CD volume. Silent when nothing feeds it.
#[inline(never)]
fn capture_peak() -> u32 {
    let mut words = [0u32; CAPTURE_WORDS];
    crate::spu_dma_read(0, &mut words);
    let (mut lo, mut hi) = (i16::MAX, i16::MIN);
    for word in words {
        for half in [word as i16, (word >> 16) as i16] {
            lo = lo.min(half);
            hi = hi.max(half);
        }
    }
    (i32::from(hi) - i32::from(lo)) as u32
}

/// Is audio alive, three ways, over a third of a second: the drive says
/// PLAYING, GetlocP advances, the capture buffer carries signal.
#[derive(Copy, Clone)]
struct Alive {
    playing: bool,
    advancing: bool,
    signal: bool,
}

#[inline(never)]
fn sample_alive(cd: &mut Cd) -> Alive {
    let s1 = stat(cd);
    let p1 = play_position(cd);
    let peak1 = capture_peak();
    let t0 = now();
    while since(t0) < HZ / 3 {}
    let s2 = stat(cd);
    let p2 = play_position(cd);
    let peak2 = capture_peak();
    let advancing = match (p1, p2) {
        (Some(a), Some(b)) => absolute_frames(&b) - absolute_frames(&a) >= 10,
        _ => false,
    };
    Alive {
        playing: s1 & STAT_PLAYING != 0 && s2 & STAT_PLAYING != 0,
        advancing,
        signal: peak1.max(peak2) >= SIGNAL,
    }
}

/// The capture buffer sampled once a VBlank while a read runs.
struct Watch {
    last_frame: u32,
    samples: u32,
    loud: u32,
    first_quiet: u32,
}

impl Watch {
    fn new() -> Self {
        Self {
            last_frame: interrupts::vblank_count(),
            samples: 0,
            loud: 0,
            first_quiet: 0,
        }
    }

    fn sample(&mut self, elapsed: u32) {
        let frame = interrupts::vblank_count();
        if frame == self.last_frame {
            return;
        }
        self.last_frame = frame;
        self.samples += 1;
        if capture_peak() >= SIGNAL {
            self.loud += 1;
        } else if self.first_quiet == 0 {
            self.first_quiet = elapsed.max(1);
        }
    }

    fn permille(&self) -> u32 {
        self.loud * 1000 / self.samples.max(1)
    }

    /// Milliseconds into the read when the first silent sample came, or
    /// 0xFFFF if every sample carried signal.
    fn quiet_ms(&self) -> u32 {
        if self.first_quiet == 0 {
            0xFFFF
        } else {
            ms(self.first_quiet)
        }
    }
}

#[inline(never)]
fn flags(bits: &[bool]) -> u32 {
    bits.iter()
        .enumerate()
        .fold(0, |acc, (i, &set)| acc | (u32::from(set) << i))
}

// --------------------------------------------------------------------- cost

/// One foreground work block: the same instructions whether a read runs or
/// not, so the difference is the CPU the transport took.
#[inline(never)]
fn spin_block(acc: &mut u32) {
    for k in 0..2048u32 {
        *acc = black_box(acc.wrapping_add(k));
    }
}

/// (iterations, HBlanks) of the foreground loop with nothing reading, with
/// the same `state` call per pass as a stream's loop.
#[inline(never)]
fn idle_window(ticket: Ticket, length: u32) -> (u32, u32) {
    let start = now();
    let mut iterations = 0u32;
    let mut acc = 0u32;
    while since(start) < length {
        spin_block(&mut acc);
        let _ = psx_cdstream::state(ticket);
        iterations += 1;
    }
    black_box(acc);
    (iterations, since(start))
}

/// Sectors read at each speed per run: 1.6 s of reading either way.
const COST_SECTORS: [u32; 2] = [240, 120];
const COST_RUNS: usize = 3;
const COST_NAMES: [&str; CDCOST_COUNT] = [
    "RATE2X X10",
    "RATE1X X10",
    "PIO US 2X",
    "PIO US 1X",
    "LOST PM 2X 1X IRQ",
    "HANDLER US 2X 1X ST",
    "FIRST MS 2X 1X DROP",
    "FLAGS CHAIN2X 1X",
];

/// What one cost run measured.
#[derive(Copy, Clone, Default)]
struct CostRun {
    ok: bool,
    /// Sectors a second, times ten, from the first sector to the last.
    rate_x10: u32,
    /// Foreground microseconds lost to each sector.
    us_per_sector: u32,
    /// Share of the foreground lost, in permille.
    lost_permille: u32,
    handler_us: u32,
    irq_x100: u32,
    first_ms: u32,
    chained: u32,
    dropped: u32,
}

#[inline(never)]
fn cost_run(ticket: Ticket, total: u32) -> CostRun {
    let (idle_iterations, idle_hblanks) = idle_window(ticket, HZ);
    psx_cdstream::reset_max_irq_ticks();
    let before = psx_cdstream::stats();
    let mut acc = 0u32;
    let out = stream(0, total, 64, &mut |_| spin_block(&mut acc));
    black_box(acc);
    let after = psx_cdstream::stats();
    // HBlanks the loop's passes would have taken with no read.
    let expected =
        (out.iterations / 16).saturating_mul(idle_hblanks) / (idle_iterations / 16).max(1);
    let lost = out.done.saturating_sub(expected);
    let after_first = out.done.saturating_sub(out.first).max(1);
    CostRun {
        ok: out.ok,
        rate_x10: (total - 1).saturating_mul(HZ).saturating_mul(10) / after_first,
        us_per_sector: lost.saturating_mul(6356) / 100 / total,
        lost_permille: lost.saturating_mul(1000) / out.done.max(1),
        handler_us: timer2_us(after.max_irq_ticks),
        irq_x100: after.irq_count.wrapping_sub(before.irq_count) * 100 / total,
        first_ms: ms(out.first),
        chained: after.chained.wrapping_sub(before.chained),
        dropped: after
            .discarded_sectors
            .wrapping_sub(before.discarded_sectors),
    }
}

/// `CD STREAM COST`: a sustained read through the transport at double then
/// single speed, three runs each.
///
/// Records from `0x2F0`, fields min/median/max unless noted:
/// 0x2F0 and 0x2F1: sectors a second times ten, 2x then 1x;
/// 0x2F2 and 0x2F3: foreground microseconds lost per sector, 2x then 1x;
/// 0x2F4: lost share in permille 2x median, 1x median, interrupts per sector
///   times a hundred (2x);
/// 0x2F5: longest handler call in microseconds 2x, 1x, handler stack bytes
///   never used;
/// 0x2F6: milliseconds to the first sector of the run (a seek back from the
///   run before) 2x median, 1x median, sectors discarded in all runs;
/// 0x2F7: flags (bit 0 transport installed, 1 all 2x runs intact, 2 all 1x
///   runs intact), requests chained at 2x, requests chained at 1x.
#[inline(never)]
pub(crate) fn run_cost(screen: &mut Screen, font: &FontAtlas) -> [TimingRecord; CDCOST_COUNT] {
    let mut ui = Ui { screen, font };
    let session = Session::open(&mut ui, "CD STREAM COST");
    let mut rates = [[0u32; COST_RUNS]; 2];
    let mut pio = [[0u32; COST_RUNS]; 2];
    let mut lost = [[0u32; COST_RUNS]; 2];
    let mut handler = [0u32; 2];
    let mut irq = [0u32; COST_RUNS];
    let mut first = [[0u32; COST_RUNS]; 2];
    let mut chained = [0u32; 2];
    let mut dropped = 0u32;
    let mut intact = [true; 2];

    // A finished request for the idle windows to ask about.
    let warm = submit(0, 1, ring(0));
    let Some(ticket) = warm else {
        session.close();
        return core::array::from_fn(|i| record(CDCOST_RECORD + i as u16, 0xFFFF, 0xFFFF, 0xFFFF));
    };
    let _ = wait(ticket);
    for (speed, double) in [true, false].into_iter().enumerate() {
        psx_cdstream::configure(config(double, true));
        let name = if double { "2X" } else { "1X" };
        for run in 0..COST_RUNS {
            let mut a = Line::new();
            a.s(name)
                .s(" RUN ")
                .u(run as u32 + 1)
                .s(" OF ")
                .u(COST_RUNS as u32);
            ui.progress("CD STREAM COST", a.as_str(), "KEEP HANDS OFF THE LID");
            let r = cost_run(ticket, COST_SECTORS[speed]);
            intact[speed] &= r.ok;
            rates[speed][run] = r.rate_x10;
            pio[speed][run] = r.us_per_sector;
            lost[speed][run] = r.lost_permille;
            handler[speed] = handler[speed].max(r.handler_us);
            first[speed][run] = r.first_ms;
            chained[speed] += r.chained;
            dropped += r.dropped;
            if speed == 0 {
                irq[run] = r.irq_x100;
            }
        }
    }
    psx_cdstream::configure(config(true, true));
    let unused = psx_cdstream::handler_stack_unused_bytes() as u32;
    session.close();

    let mut spreads = [(0, 0, 0); 8];
    for s in 0..2 {
        spreads[s] = spread(&mut rates[s]);
        spreads[2 + s] = spread(&mut pio[s]);
        spreads[4 + s] = spread(&mut lost[s]);
        spreads[6 + s] = spread(&mut first[s]);
    }
    let irq_med = spread(&mut irq).1;
    let ok = intact[0] && intact[1];
    let records = [
        record(CDCOST_RECORD, spreads[0].0, spreads[0].1, spreads[0].2),
        record(CDCOST_RECORD + 1, spreads[1].0, spreads[1].1, spreads[1].2),
        record(CDCOST_RECORD + 2, spreads[2].0, spreads[2].1, spreads[2].2),
        record(CDCOST_RECORD + 3, spreads[3].0, spreads[3].1, spreads[3].2),
        record(CDCOST_RECORD + 4, spreads[4].1, spreads[5].1, irq_med),
        record(CDCOST_RECORD + 5, handler[0], handler[1], unused),
        record(CDCOST_RECORD + 6, spreads[6].1, spreads[7].1, dropped),
        record(
            CDCOST_RECORD + 7,
            flags(&[true, intact[0], intact[1]]),
            chained[0],
            chained[1],
        ),
    ];
    ui.table("CD STREAM COST", ok, &COST_NAMES, &records, &[]);
    records
}

// ------------------------------------------------------------------ handoff

/// Sectors of the data read that follows audio.
const AFTER_AUDIO_SECTORS: u32 = 64;
const HANDOFF_REPS: usize = 3;
const HANDOFF_NAMES: [&str; CDHANDOFF_COUNT] = [
    "LEASE MS",
    "T PAUSE X10",
    "ACK X10 STAT STILL",
    "1ST SECTOR MS",
    "64 SECT MS",
    "RESUME MS",
    "MOVED INPLACE PLAY",
    "RECOV 1ST 64 CODE",
    "RECOV FLAG PM QUIET",
    "BARE 1ST 64 CODE",
    "BARE FLAG PM QUIET",
    "CTRL PM N FLAGS",
];

/// A read that starts while the tone plays, with evidence about the audio.
#[derive(Copy, Clone, Default)]
struct OverAudio {
    ran: bool,
    control: (bool, bool, bool),
    after: (bool, bool, bool),
    read_ok: bool,
    code: u32,
    first_ms: u32,
    done_ms: u32,
    loud_permille: u32,
    samples: u32,
    quiet_ms: u32,
}

impl OverAudio {
    fn flags(&self) -> u32 {
        flags(&[
            self.control.0,
            self.control.1,
            self.control.2,
            self.after.0,
            self.after.1,
            self.after.2,
            self.read_ok,
            self.ran,
        ])
    }
}

/// Start the tone, prove it is alive, give the drive back with the audio
/// still playing, read, and look at the audio again. `recovery` is the
/// transport's own Pause before the seek.
#[inline(never)]
fn read_over_audio(ui: &mut Ui, recovery: bool) -> OverAudio {
    let mut result = OverAudio::default();
    let mut a = Line::new();
    a.s("READ OVER PLAYING TONE");
    ui.progress(
        "CD-DA HANDOFF",
        a.as_str(),
        if recovery {
            "RECOVERY PAUSE ON"
        } else {
            "NO PAUSE AT ALL"
        },
    );
    psx_cdstream::configure(config(true, recovery));
    if let Some(mut cd) = acquire_audio() {
        let _ = start_tone(&mut cd);
        let t0 = now();
        while since(t0) < HZ {}
        let alive = sample_alive(&mut cd);
        result.control = (alive.playing, alive.advancing, alive.signal);
        release_audio(cd);
        result.ran = true;
        let mut watch = Watch::new();
        let out = stream(100, AFTER_AUDIO_SECTORS, 1, &mut |t| watch.sample(t));
        result.read_ok = out.ok;
        result.code = out.code;
        result.first_ms = ms(out.first);
        result.done_ms = ms(out.done);
        result.loud_permille = watch.permille();
        result.samples = watch.samples;
        result.quiet_ms = watch.quiet_ms();
        if let Some(mut cd) = acquire_audio() {
            let alive = sample_alive(&mut cd);
            result.after = (alive.playing, alive.advancing, alive.signal);
            release_audio(cd);
        }
    }
    quiet();
    psx_cdstream::configure(config(true, true));
    result
}

/// `CD-DA HANDOFF`: the audio lease and what a data read does to CD-DA.
///
/// Records from `0x300`, fields min/median/max unless noted (times in
/// milliseconds, "x10" in tenths):
/// 0x300: lease request to granted while a read runs;
/// 0x301: Pause complete after CD-DA (`T_pause`), x10;
/// 0x302: Pause acknowledge x10 (median), drive status byte after the Pause
///   (last), 1 if GetlocP stood still after it;
/// 0x303: first data sector after audio (Pause then read);
/// 0x304: 64 data sectors landed, after audio;
/// 0x305: Play after SetLoc to the drive reporting PLAYING;
/// 0x306: resume position: frames moved from the saved position (median,
///   two's complement), runs that resumed in place (of 3), runs PLAYING;
/// 0x307, 0x309: read over playing audio, with the recovery Pause (0x307)
///   and without any Pause (0x309): first sector ms, 64 sectors ms, failure
///   code (0 intact);
/// 0x308, 0x30A: the audio in those two runs: flags (bit 0 PLAYING before,
///   1 GetlocP advancing before, 2 capture signal before, 3 PLAYING after,
///   4 advancing after, 5 signal after, 6 read intact, 7 ran), share of
///   VBlank samples of the SPU CD capture buffer that carried signal during
///   the read (permille), milliseconds into the read of the first silent
///   sample (0xFFFF none);
/// 0x30B: control after a proper Pause: signal share during the read
///   (permille, expect 0), samples taken, flags (bit 0 PLAYING before the
///   Pause, 1 advancing, 2 capture signal, 3 PLAYING after the Pause).
#[inline(never)]
pub(crate) fn run_handoff(
    screen: &mut Screen,
    font: &FontAtlas,
) -> [TimingRecord; CDHANDOFF_COUNT] {
    let mut ui = Ui { screen, font };
    let session = Session::open(&mut ui, "CD-DA HANDOFF");

    // 1. How fast a lease request stops a read in flight.
    let mut lease = [0u32; 4];
    let mut lease_n = 0;
    for rep in 0..lease.len() {
        let mut a = Line::new();
        a.s("LEASE WHILE READING ").u(rep as u32 + 1);
        ui.progress("CD-DA HANDOFF", a.as_str(), "");
        wait_idle();
        let mut tickets = [None; SLOTS];
        for (slot, t) in tickets.iter_mut().enumerate() {
            *t = submit(8 * slot as u32, PER, ring(slot));
        }
        let Some(first) = tickets[0] else { continue };
        let start = now();
        let mut reached = false;
        while since(start) < PATIENCE {
            if matches!(psx_cdstream::state(first), RequestState::Active { received } if received >= 2)
            {
                reached = true;
                break;
            }
        }
        if reached {
            let asked = now();
            let _ = psx_cdstream::request_audio_lease();
            while psx_cdstream::lease_state() != LeaseState::Granted && since(asked) < PATIENCE {}
            lease[lease_n] = since(asked);
            lease_n += 1;
        }
        if let Some(cd) = psx_cdstream::take_audio_lease() {
            release_audio(cd);
        }
        psx_cdstream::cancel_all();
        wait_idle();
    }
    let lease_spread = spread(&mut lease[..lease_n]);

    // 2. The proper hand-off, three times: Pause the tone, read, resume.
    let mut t_pause = [0u32; HANDOFF_REPS];
    let mut t_ack = [0u32; HANDOFF_REPS];
    let mut first = [0u32; HANDOFF_REPS];
    let mut done = [0u32; HANDOFF_REPS];
    let mut resume = [0u32; HANDOFF_REPS];
    let mut moved = [0i32; HANDOFF_REPS];
    let mut in_place = 0u32;
    let mut resumed = 0u32;
    let mut stat_paused = 0u8;
    let mut held_still = true;
    let mut reads_ok = true;
    let mut control = (false, false, false);
    let mut watch = Watch::new();
    for rep in 0..HANDOFF_REPS {
        let mut a = Line::new();
        a.s("PAUSE, READ, RESUME ").u(rep as u32 + 1);
        ui.progress("CD-DA HANDOFF", a.as_str(), "");
        let mut saved = None;
        if let Some(mut cd) = acquire_audio() {
            let _ = start_tone(&mut cd);
            let t0 = now();
            while since(t0) < 2 * HZ {}
            if rep == 0 {
                let alive = sample_alive(&mut cd);
                control = (alive.playing, alive.advancing, alive.signal);
            }
            if let Some(t) = timed_command(&mut cd, CMD_PAUSE, &[], true, 4 * HZ) {
                t_ack[rep] = t.ack;
                t_pause[rep] = t.complete;
            }
            stat_paused = stat(&mut cd);
            saved = play_position(&mut cd);
            let t1 = now();
            while since(t1) < HZ / 4 {}
            if let (Some(a), Some(b)) = (saved, play_position(&mut cd)) {
                held_still &= absolute_frames(&b) == absolute_frames(&a);
            }
            release_audio(cd);
        }
        let out = stream(100, AFTER_AUDIO_SECTORS, 1, &mut |t| watch.sample(t));
        reads_ok &= out.ok;
        first[rep] = out.first;
        done[rep] = out.done;
        if let (Some(saved), Some(mut cd)) = (saved, acquire_audio()) {
            let (latency, playing) = resume_at(&mut cd, &saved);
            resume[rep] = latency;
            let t0 = now();
            while since(t0) < 3 * HZ / 2 {}
            if let Some(at) = play_position(&mut cd) {
                moved[rep] = absolute_frames(&at) - absolute_frames(&saved);
                // Resumed in place: ahead of the saved position by about the
                // time since, not back at the top of the track.
                if moved[rep] >= 0 && moved[rep] <= 75 * 6 {
                    in_place += 1;
                }
            }
            resumed += u32::from(playing);
            release_audio(cd);
        }
        quiet();
    }
    let t_pause_spread = spread(&mut t_pause);
    let ack_med = spread(&mut t_ack).1;
    let first_spread = spread(&mut first);
    let done_spread = spread(&mut done);
    let resume_spread = spread(&mut resume);
    let mut moved_sorted = [0u32; HANDOFF_REPS];
    for (i, m) in moved.iter().enumerate() {
        moved_sorted[i] = (*m + 0x8000) as u32;
    }
    let moved_med = (spread(&mut moved_sorted).1 as i32 - 0x8000) as i16 as u16 as u32;

    // 3 and 4. A read that starts while the tone plays.
    let with_recovery = read_over_audio(&mut ui, true);
    let bare = read_over_audio(&mut ui, false);
    // The transport has to read afterwards whatever that did.
    let after = read_small(100, 1);
    session.close();

    let ok = reads_ok && with_recovery.read_ok && after.ok && lease_n > 0;
    let control_flags = flags(&[
        control.0,
        control.1,
        control.2,
        stat_paused & STAT_PLAYING != 0,
    ]);
    let records = [
        record(
            CDHANDOFF_RECORD,
            ms(lease_spread.0),
            ms(lease_spread.1),
            ms(lease_spread.2),
        ),
        record(
            CDHANDOFF_RECORD + 1,
            ms10(t_pause_spread.0),
            ms10(t_pause_spread.1),
            ms10(t_pause_spread.2),
        ),
        record(
            CDHANDOFF_RECORD + 2,
            ms10(ack_med),
            u32::from(stat_paused),
            u32::from(held_still),
        ),
        record(
            CDHANDOFF_RECORD + 3,
            ms(first_spread.0),
            ms(first_spread.1),
            ms(first_spread.2),
        ),
        record(
            CDHANDOFF_RECORD + 4,
            ms(done_spread.0),
            ms(done_spread.1),
            ms(done_spread.2),
        ),
        record(
            CDHANDOFF_RECORD + 5,
            ms(resume_spread.0),
            ms(resume_spread.1),
            ms(resume_spread.2),
        ),
        record(CDHANDOFF_RECORD + 6, moved_med, in_place, resumed),
        record(
            CDHANDOFF_RECORD + 7,
            with_recovery.first_ms,
            with_recovery.done_ms,
            code16(with_recovery.code),
        ),
        record(
            CDHANDOFF_RECORD + 8,
            with_recovery.flags(),
            with_recovery.loud_permille,
            with_recovery.quiet_ms,
        ),
        record(
            CDHANDOFF_RECORD + 9,
            bare.first_ms,
            bare.done_ms,
            code16(bare.code),
        ),
        record(
            CDHANDOFF_RECORD + 10,
            bare.flags(),
            bare.loud_permille,
            bare.quiet_ms,
        ),
        record(
            CDHANDOFF_RECORD + 11,
            watch.permille(),
            watch.samples,
            control_flags,
        ),
    ];
    let mut extra = [Line::new(); 4];
    extra[0]
        .s("RESUMED IN PLACE ")
        .u(in_place)
        .s(" OF 3  HELD STILL ")
        .yn(held_still);
    extra[1]
        .s("TONE ALIVE BEFORE ")
        .yn(control.0 && control.1 && control.2);
    extra[2]
        .s("RECOVERY: SIGNAL ")
        .u(with_recovery.loud_permille / 10)
        .s("% AFTER ")
        .yn(with_recovery.after.2);
    extra[3]
        .s("NO PAUSE: SIGNAL ")
        .u(bare.loud_permille / 10)
        .s("% AFTER ")
        .yn(bare.after.2);
    ui.table("CD-DA HANDOFF", ok, &HANDOFF_NAMES, &records, &extra);
    records
}

// -------------------------------------------------------------------- motor

const MOTOR_NAMES: [&str; CDMOTOR_COUNT] = [
    "PAUSE+0S 1ST 4 CODE",
    "PAUSE+5S 1ST 4 CODE",
    "PAUSE+15S 1ST 4 CODE",
    "STOP ACK ATONCE CODE",
    "STOP DONE OFF SETTLED",
    "FLAGS RECOVER 1ST",
];

/// A read after waiting, as a record: first sector ms, 4 sectors ms, code.
#[inline(never)]
fn motor_read(start: u32) -> (u32, u32, u32, bool) {
    let out = read_small(start, PER);
    (ms(out.first), ms(out.done), code16(out.code), out.ok)
}

/// `CD MOTOR`: what the drive does to a read after Pause and after Stop.
///
/// Records from `0x310`, first sector ms, 4 sectors ms, failure code
/// (0 intact, 0xFFFF timeout) unless noted:
/// 0x310, 0x311, 0x312: a read after a Pause and a wait of 0, 5 and 15 s;
/// 0x313: Stop acknowledge ms, the read right after the Stop (4 sectors ms,
///   0xFFFF if it failed), its failure code;
/// 0x314: Stop complete ms, motor reported off after ms (0xFFFF never within
///   8 s), the read once the motor stopped (4 sectors ms);
/// 0x315: flags (bit 0 read right after Stop intact, 1 read after the
///   settled Stop intact, 2 the transport read again afterwards), that last
///   read's 4 sectors ms, the first sector ms of the read right after Stop.
#[inline(never)]
pub(crate) fn run_motor(screen: &mut Screen, font: &FontAtlas) -> [TimingRecord; CDMOTOR_COUNT] {
    let mut ui = Ui { screen, font };
    let session = Session::open(&mut ui, "CD MOTOR");
    let mut gaps = [(0, 0, 0, false); 3];
    for (i, seconds) in [0u32, 5, 15].into_iter().enumerate() {
        let mut a = Line::new();
        a.s("PAUSE THEN WAIT ").u(seconds).s("S");
        ui.progress("CD MOTOR", a.as_str(), "");
        let _ = read_small(300, 1);
        delay(seconds * HZ);
        gaps[i] = motor_read(300 + 1 + 128);
    }

    ui.progress("CD MOTOR", "STOP THEN READ AT ONCE", "");
    let _ = read_small(300, 1);
    let mut stop_ack = 0;
    let mut at_once = (0, 0xFFFF, 0xFFFF, false);
    if let Some(mut cd) = acquire_audio() {
        if let Some(t) = timed_command(&mut cd, CMD_STOP, &[], false, 3 * HZ) {
            stop_ack = t.ack;
        }
        release_audio(cd);
        at_once = motor_read(300 + 1 + 128);
    }
    // Whatever that did, the transport must read afterwards.
    let recovered = read_small(300, PER);

    ui.progress("CD MOTOR", "STOP, WAIT FOR THE MOTOR", "");
    let mut stop_complete = 0;
    let mut motor_off = 0xFFFF;
    let mut settled = (0, 0xFFFF, 0xFFFF, false);
    if let Some(mut cd) = acquire_audio() {
        let t0 = now();
        if let Some(t) = timed_command(&mut cd, CMD_STOP, &[], true, 8 * HZ) {
            stop_complete = t.complete;
        }
        while since(t0) < 8 * HZ {
            if stat(&mut cd) & STAT_MOTOR_ON == 0 {
                motor_off = ms(since(t0));
                break;
            }
        }
        release_audio(cd);
        settled = motor_read(300 + 1 + 128);
    }
    session.close();

    let ok = gaps.iter().all(|g| g.3) && recovered.ok;
    let records = [
        record(CDMOTOR_RECORD, gaps[0].0, gaps[0].1, gaps[0].2),
        record(CDMOTOR_RECORD + 1, gaps[1].0, gaps[1].1, gaps[1].2),
        record(CDMOTOR_RECORD + 2, gaps[2].0, gaps[2].1, gaps[2].2),
        record(CDMOTOR_RECORD + 3, ms(stop_ack), at_once.1, at_once.2),
        record(CDMOTOR_RECORD + 4, ms(stop_complete), motor_off, settled.1),
        record(
            CDMOTOR_RECORD + 5,
            flags(&[at_once.3, settled.3, recovered.ok]),
            ms(recovered.done),
            at_once.0,
        ),
    ];
    ui.table("CD MOTOR", ok, &MOTOR_NAMES, &records, &[]);
    records
}

/// Run one of the CD STREAM cases (or all three) on `gpu`, leaving its
/// records in `results`.
#[inline(never)]
pub(crate) fn run(
    gpu: &mut psx_gpu::Gpu,
    results: &mut crate::console_tests::Results,
    case: ConsoleCase,
) {
    let font = FontAtlas::upload(&psx_font::fonts::BASIC, crate::FONT_TPAGE, crate::FONT_CLUT);
    let mut screen = Screen::new(gpu);
    let all = case == ConsoleCase::CdAll;
    if all || case == ConsoleCase::CdCost {
        results.cd_cost = Some(run_cost(&mut screen, &font));
    }
    if all || case == ConsoleCase::CdHandoff {
        results.cd_handoff = Some(run_handoff(&mut screen, &font));
    }
    if all || case == ConsoleCase::CdMotor {
        results.cd_motor = Some(run_motor(&mut screen, &font));
    }
}
