// SPDX-License-Identifier: GPL-2.0-or-later
//! CD RADIUS SWEEP (v2.8): does this console read a full 80-minute CD-R all the
//! way out?
//!
//! The radius disc (`make hwtest-radius-disc`) carries `RADIUS.DAT`, a file of
//! deterministic filler (`filler.rs`) that fills the data track out to about
//! 78 minutes, as full as the demo disc. This step reads fixed groups of
//! [`GROUP`] sectors at positions across it: the file's first sector, every
//! fourth minute, and the last two minutes every twenty seconds, and checks
//! every word of every sector against the filler's formula.
//!
//! Each position is read three ways, one after the other:
//!
//! * **S**: the SDK's polled `SectorReader` at double speed, the path the
//!   demo-disc loader and every game's chunk loader use.
//! * **2**: raw controller commands (SetMode, Setloc, ReadN, our own flag
//!   polling and PIO pops of the data FIFO) at double speed.
//! * **1**: the same raw commands at single speed, the control: a position
//!   that fails at 2 and passes at 1 is a speed-margin problem.
//!
//! Per group the step counts sectors delivered, sectors whose data matched,
//! INT5 drive errors (and the first error code), timeouts, retries (a group
//! that fails is restarted from the sector it stopped at, up to
//! [`ATTEMPTS`] attempts) and the elapsed time, which includes the seek (the
//! first method at a position pays the long seek from the previous one, the
//! next two read back the same place). A group checksum (FNV-1a over the words
//! received) goes in the record so the host can recompute it.
//!
//! Every wait is bounded: a flag poll gives up after [`IDLE_TICKS`] of
//! silence, a group after [`ATTEMPTS`] attempts, and the whole sweep after
//! [`DEAD_LIMIT`] positions in a row where nothing could be read at all. SELECT
//! skips the rest of the sweep.
//!
//! The result table is drawn row by row while the step runs, kept in
//! [`TABLE`], and shown as capture pages after the run (`draw_page`).
//!
//! Records from `0xB00`:
//! `0xB00`: positions planned, positions done, sectors a group;
//! `0xB01`: file first LBA low half, high half, 0;
//! `0xB02`: file size in sectors low half, high half, positions given up on (dead);
//! `0xB10 + 8 * position + 2 * method` (method 0 SDK, 1 raw 2x, 2 raw 1x):
//!   delivered sectors, matching sectors, centiseconds;
//! `0xB11 + 8 * position + 2 * method`:
//!   INT5 count (high byte) and timeouts (low byte), retries (high byte) and
//!   first INT5 error code (low byte), group checksum folded to 16 bits;
//! `0xB16 + 8 * position`: position first LBA low half, high half, state codes
//!   of the three methods (2 bits each, [`Cell::code`]).

use crate::console_tests::record;
use crate::filler::{self, FNV_START};
use crate::xa_loop::{self, Clock};
use crate::{bounds, ui, TimingRecord};
use core::ptr::{addr_of, addr_of_mut};
use psx_font::FontAtlas;
use psx_io::periph::Cd;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::tty;

/// First record id.
pub(crate) const RADIUS_RECORD: u16 = 0xB00;
const POSITION_RECORD: u16 = 0xB10;
/// Records per position (three methods of two, and the position's own).
const RECORDS_PER_POSITION: u16 = 8;

/// The file the radius disc carries.
pub(crate) const FILLER_FILE: &str = "RADIUS.DAT";
/// Sectors in a group.
const GROUP: u32 = 64;
/// Positions the table holds.
pub(crate) const MAX_POSITIONS: usize = 28;
const METHODS: usize = 3;
/// Attempts at a group (the first and up to two retries).
const ATTEMPTS: u32 = 3;
/// Silence after which a raw read gives up: HBlank ticks, about 2.5 seconds
/// (a full-stroke seek on a worn disc is under a second).
const IDLE_TICKS: u32 = 39_000;
/// Positions in a row with nothing readable that end the sweep.
const DEAD_LIMIT: u32 = 3;
/// Spin budget of the single commands (a hang guard, not a latency).
const SPINS: u32 = 200_000;
/// Poll passes for the data FIFO to report data after the request.
const FIFO_SPINS: u32 = 2_000_000;
const HBLANKS_PER_SECOND: u32 = 15_734;
/// Frames per minute mark and the dense tail: minutes between marks, and the
/// dense tail's spacing in sectors (20 seconds) and length (two minutes).
const MARK_MINUTES: u32 = 4;
const TAIL_STEP: u32 = 1_500;
const TAIL_SECTORS: u32 = 9_000;

/// How a cell ended, in the order the table shows them.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Code {
    /// Not run.
    None,
    /// All sectors, all data right, nothing to report.
    Ok,
    /// All sectors and data right, after retries or with an error on the way.
    Retried,
    /// Sectors came, some data was wrong.
    Wrong,
    /// Fewer sectors than asked and the last event was a timeout.
    Timeout,
    /// Fewer sectors than asked and the drive reported INT5.
    DriveError,
}

impl Code {
    const fn bits(self) -> u32 {
        match self {
            Self::None | Self::Ok => 0,
            Self::Retried => 1,
            Self::Wrong => 2,
            Self::Timeout | Self::DriveError => 3,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::None => "--",
            Self::Ok => "OK",
            Self::Retried => "RT",
            Self::Wrong => "XX",
            Self::Timeout => "TO",
            Self::DriveError => "ER",
        }
    }
}

/// One method at one position.
#[derive(Copy, Clone)]
pub(crate) struct Cell {
    pub(crate) code: Code,
    delivered: u32,
    good: u32,
    int5: u32,
    timeouts: u32,
    retries: u32,
    /// First INT5 error code the drive gave (0 none).
    error: u8,
    centis: u32,
    fold: u32,
}

impl Cell {
    const EMPTY: Self = Self {
        code: Code::None,
        delivered: 0,
        good: 0,
        int5: 0,
        timeouts: 0,
        retries: 0,
        error: 0,
        centis: 0,
        fold: 0,
    };
}

#[derive(Copy, Clone)]
pub(crate) struct Position {
    lba: u32,
    cells: [Cell; METHODS],
}

const BLANK: Position = Position {
    lba: 0,
    cells: [Cell::EMPTY; METHODS],
};

/// The results, shown as pages after the run.
pub(crate) static mut TABLE: [Position; MAX_POSITIONS] = [BLANK; MAX_POSITIONS];
static mut TABLE_LEN: usize = 0;
/// Positions given up on after [`DEAD_LIMIT`] dead ones in a row.
static mut DEAD_SKIPPED: u32 = 0;
/// Whether the filler file was on the disc.
static mut FILE_FOUND: bool = false;

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

// ------------------------------------------------------------------ plan

/// Where the groups go: the file's first sector, every [`MARK_MINUTES`]th
/// minute of disc time inside it, then the dense tail ending on the last full
/// group. Returns the positions' first LBAs.
fn plan(first: u32, sectors: u32) -> ([u32; MAX_POSITIONS], usize) {
    let mut out = [0u32; MAX_POSITIONS];
    let mut count = 0;
    let end = first + sectors;
    let tail_from = end.saturating_sub(TAIL_SECTORS + GROUP);
    let mut push = |lba: u32| {
        // At least a group between positions; the table has room for all.
        if count < MAX_POSITIONS && (count == 0 || lba >= out[count - 1] + GROUP) {
            out[count] = lba;
            count += 1;
        }
    };
    push(first);
    let mut minute = MARK_MINUTES;
    loop {
        // Absolute time m:00 is frame m * 4500, and LBA 0 is frame 150.
        let lba = minute * 4_500 - 150;
        if lba >= tail_from {
            break;
        }
        if lba >= first + GROUP {
            push(lba);
        }
        minute += MARK_MINUTES;
    }
    let mut at = tail_from;
    while at + GROUP < end {
        push(at);
        at += TAIL_STEP;
    }
    push(end - GROUP);
    (out, count)
}

// ---------------------------------------------------------------- reading

/// What a read of one group collected.
struct Tally {
    delivered: u32,
    good: u32,
    int5: u32,
    timeouts: u32,
    retries: u32,
    error: u8,
    fold: u32,
    /// The last thing that went wrong: 0 nothing, 1 timeout, 2 drive error.
    last: u8,
}

impl Tally {
    const fn new() -> Self {
        Self {
            delivered: 0,
            good: 0,
            int5: 0,
            timeouts: 0,
            retries: 0,
            error: 0,
            fold: FNV_START,
            last: 0,
        }
    }

    fn drive_error(&mut self, code: u8) {
        self.int5 += 1;
        if self.error == 0 {
            self.error = code;
        }
        self.last = 2;
    }

    fn timeout(&mut self) {
        self.timeouts += 1;
        self.last = 1;
    }

    /// Check the sector in [`SECTOR`] against the filler's sector `index`.
    #[inline(never)]
    fn check(&mut self, index: u32) {
        // SAFETY: single thread; the buffer is this module's.
        let buffer = unsafe { &*addr_of!(SECTOR) };
        let mut wrong = 0u32;
        let mut fold = self.fold;
        for (i, &word) in buffer.iter().enumerate() {
            if word != filler::word(index, i as u32) {
                wrong += 1;
            }
            fold = filler::fnv_step(fold, word);
        }
        self.fold = fold;
        self.delivered += 1;
        if wrong == 0 {
            self.good += 1;
        }
    }

    fn cell(&self, clock: &mut Clock) -> Cell {
        let ticks = clock.ticks();
        let centis = ticks / (HBLANKS_PER_SECOND / 100);
        let code = if self.delivered >= GROUP {
            if self.good < GROUP {
                Code::Wrong
            } else if self.retries > 0 || self.int5 > 0 || self.timeouts > 0 {
                Code::Retried
            } else {
                Code::Ok
            }
        } else if self.last == 2 {
            Code::DriveError
        } else {
            Code::Timeout
        };
        Cell {
            code,
            delivered: self.delivered,
            good: self.good,
            int5: self.int5,
            timeouts: self.timeouts,
            retries: self.retries,
            error: self.error,
            centis,
            fold: self.fold,
        }
    }
}

/// The SDK reader, as the loader runs it: prepare (double speed), start,
/// pop sectors, stop; a failed group restarts from the sector it reached.
#[inline(never)]
fn read_sdk(lba: u32, first: u32) -> Cell {
    let mut clock = Clock::start();
    let mut tally = Tally::new();
    let mut attempt = 0;
    // SAFETY: single thread; nothing else drives the controller.
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    // SAFETY: as above; the buffer is this module's.
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    while tally.delivered < GROUP && attempt < bounds::scale(ATTEMPTS) {
        attempt += 1;
        if attempt > 1 {
            tally.retries += 1;
        }
        let from = lba + tally.delivered;
        if !reader.prepare() {
            tally.timeout();
            reader.stop();
            continue;
        }
        if !reader.start_read(from) {
            note_failure(&mut tally, reader.diagnostics());
            reader.stop();
            continue;
        }
        while tally.delivered < GROUP {
            if reader.read_sector(sector) {
                tally.check(lba + tally.delivered - first);
            } else {
                note_failure(&mut tally, reader.diagnostics());
                break;
            }
        }
        reader.stop();
    }
    tally.cell(&mut clock)
}

/// The SDK reader's failure snapshot is `cause << 24 | status << 16 | command
/// << 8 | flag_or_error`: cause 5 is INT5 with the error code in the low byte.
fn note_failure(tally: &mut Tally, diag: u32) {
    if (diag >> 24) as u8 == 0x05 {
        tally.drive_error(diag as u8);
    } else {
        tally.timeout();
    }
}

/// The controller's response bytes, status first, drained.
fn take_response(cd: &mut Cd) -> (u8, u8) {
    let mut bytes = [0u8; 2];
    for n in 0..16 {
        if cd.status_register() & psx_hw::cd::index_status::RESPONSE_NOT_EMPTY == 0 {
            break;
        }
        let byte = cd.read_response_byte();
        if n < 2 {
            bytes[n] = byte;
        }
    }
    (bytes[0], bytes[1])
}

/// Pop one sector from the data FIFO into [`SECTOR`] with single-byte reads.
/// False when the FIFO never reported data.
fn pop_sector(cd: &mut Cd) -> bool {
    cd.request_data();
    let mut spins = bounds::scale(FIFO_SPINS);
    while !cd.is_data_fifo_ready() {
        if spins == 0 {
            return false;
        }
        spins -= 1;
    }
    // SAFETY: single thread; the buffer is this module's.
    let buffer = unsafe { &mut *addr_of_mut!(SECTOR) };
    for word in buffer.iter_mut() {
        let b0 = cd.read_data_byte() as u32;
        let b1 = cd.read_data_byte() as u32;
        let b2 = cd.read_data_byte() as u32;
        let b3 = cd.read_data_byte() as u32;
        // SAFETY: `word` is a live, aligned `&mut u32`.
        unsafe { core::ptr::write_volatile(word, (b3 << 24) | (b2 << 16) | (b1 << 8) | b0) };
    }
    true
}

/// Leave the drive paused with no flag waiting.
fn quiet(cd: &mut Cd) {
    let _ = cd.try_pause_until_complete(SPINS);
    for _ in 0..8 {
        let flag = cd.irq_flag_value();
        if flag == 0 {
            break;
        }
        cd.discard_response();
        cd.acknowledge_irq(flag);
    }
}

/// Raw commands at `mode` (0x80 double speed, 0 single): SetMode, Setloc,
/// ReadN, then flag polling and PIO pops, our own bounded loop.
#[inline(never)]
fn read_raw(mode: u8, lba: u32, first: u32) -> Cell {
    let mut clock = Clock::start();
    let mut tally = Tally::new();
    let mut attempt = 0;
    // SAFETY: nothing else drives the controller during the step.
    let mut cd = unsafe { Cd::steal() };
    while tally.delivered < GROUP && attempt < bounds::scale(ATTEMPTS) {
        attempt += 1;
        if attempt > 1 {
            tally.retries += 1;
        }
        let from = lba + tally.delivered;
        quiet(&mut cd);
        if cd.try_set_mode(mode, SPINS).is_none()
            || cd.try_set_target_lba(from, SPINS).is_none()
            || cd.try_start_reading(SPINS).is_none()
        {
            // The command was refused: INT5 or no answer. The status register
            // tells which only by the flag the controller left.
            if cd.irq_flag_value() == 5 {
                let (_, code) = take_response(&mut cd);
                cd.acknowledge_irq(5);
                tally.drive_error(code);
            } else {
                tally.timeout();
            }
            continue;
        }
        let mut idle_since = clock.ticks();
        let mut passes = 0u32;
        let pass_cap = bounds::scale(60_000_000);
        loop {
            passes += 1;
            let now = clock.ticks();
            if passes > pass_cap || now.wrapping_sub(idle_since) > IDLE_TICKS {
                tally.timeout();
                break;
            }
            match cd.irq_flag_value() {
                0 => {}
                1 => {
                    let popped = pop_sector(&mut cd);
                    cd.discard_response();
                    cd.acknowledge_irq(1);
                    if popped {
                        tally.check(lba + tally.delivered - first);
                        idle_since = now;
                        if tally.delivered >= GROUP {
                            break;
                        }
                    } else {
                        tally.timeout();
                        break;
                    }
                }
                5 => {
                    let (_, code) = take_response(&mut cd);
                    cd.acknowledge_irq(5);
                    tally.drive_error(code);
                    break;
                }
                other => {
                    cd.discard_response();
                    cd.acknowledge_irq(other);
                }
            }
        }
        quiet(&mut cd);
    }
    quiet(&mut cd);
    tally.cell(&mut clock)
}

// ----------------------------------------------------------------- the step

/// `MM:SS` of the disc time of `lba` (LBA 0 is 00:02:00).
fn clock_text(line: &mut ui::Line, lba: u32) {
    let frames = lba + 150;
    let minutes = frames / 4_500;
    let seconds = frames % 4_500 / 75;
    pad2(line, minutes);
    line.s(":");
    pad2(line, seconds);
}

fn pad2(line: &mut ui::Line, value: u32) {
    if value < 10 {
        line.s("0");
    }
    line.u(value);
}

/// `S.CC`, seconds with two decimals, capped.
fn seconds_text(line: &mut ui::Line, centis: u32) {
    if centis >= 10_000 {
        line.s("99.99");
        return;
    }
    line.u(centis / 100).s(".");
    pad2(line, centis % 100);
}

/// One result row as the table shows it.
fn row_text(line: &mut ui::Line, position: &Position) {
    clock_text(line, position.lba);
    for (cell, tag) in position.cells.iter().zip([" S ", " 2 ", " 1 "]) {
        line.s(tag).s(cell.code.text()).s(" ");
        seconds_text(line, cell.centis);
    }
}

fn colour_of(position: &Position) -> (u8, u8, u8) {
    let worst = position
        .cells
        .iter()
        .map(|cell| match cell.code {
            Code::None | Code::Ok => 0,
            Code::Retried => 1,
            _ => 2,
        })
        .max()
        .unwrap_or(0);
    match worst {
        0 => (150, 230, 160),
        1 => (255, 216, 96),
        _ => (255, 110, 110),
    }
}

/// The step. Fills [`TABLE`] and returns the records.
pub(crate) fn run(font: &FontAtlas) -> RadiusRecords {
    let mut out = RadiusRecords::new();
    // SAFETY: single thread; plain statics.
    unsafe {
        TABLE_LEN = 0;
        DEAD_SKIPPED = 0;
        FILE_FOUND = false;
        *addr_of_mut!(TABLE) = [BLANK; MAX_POSITIONS];
    }
    let found = xa_loop::locate(FILLER_FILE);
    let Some((first, bytes)) = found else {
        tty::println("hardware-tests: radius: RADIUS.DAT not on this disc");
        ui::live_line(font, 0, "NO RADIUS.DAT ON THIS DISC.", (255, 216, 96));
        ui::live_line(font, 1, "USE THE RADIUS DISC IMAGE.", (255, 216, 96));
        out.push(record(RADIUS_RECORD, 0, 0, GROUP));
        return out;
    };
    let sectors = bytes / 2_048;
    // SAFETY: single thread; plain static.
    unsafe { FILE_FOUND = true };
    let (lbas, count) = plan(first, sectors);
    out.push(record(RADIUS_RECORD + 1, first & 0xFFFF, first >> 16, 0));
    tty::print("hardware-tests: radius file at ");
    print_dec(first);
    tty::print(" sectors ");
    print_dec(sectors);
    tty::print(" positions ");
    print_dec(count as u32);
    tty::print("\n");
    let mut dead_run = 0u32;
    let mut done = 0usize;
    for (index, &lba) in lbas[..count].iter().enumerate() {
        bounds::poll();
        if bounds::skipping() || dead_run >= DEAD_LIMIT {
            // SAFETY: single thread; plain static.
            unsafe { DEAD_SKIPPED = (count - index) as u32 };
            break;
        }
        let mut position = Position {
            lba,
            cells: [Cell::EMPTY; METHODS],
        };
        let mut any = false;
        for method in 0..METHODS {
            let id = POSITION_RECORD + RECORDS_PER_POSITION * index as u16 + 2 * method as u16;
            bounds::record_start(id);
            let mut tag = ui::Line::new();
            clock_text(&mut tag, lba);
            tag.s([" SDK 2X", " RAW 2X", " RAW 1X"][method]);
            ui::detail(font, "RADIUS", tag.as_str());
            psx_io::diag::step(0xB00 + index as u32, "RADIUS");
            let cell = match method {
                0 => read_sdk(lba, first),
                1 => read_raw(0x80, lba, first),
                _ => read_raw(0x00, lba, first),
            };
            position.cells[method] = cell;
            any |= cell.delivered > 0;
            tty_cell(index, method, &cell);
        }
        dead_run = if any { 0 } else { dead_run + 1 };
        // SAFETY: single thread; plain statics.
        unsafe {
            (*addr_of_mut!(TABLE))[index] = position;
            TABLE_LEN = index + 1;
        }
        done = index + 1;
        let mut line = ui::Line::new();
        row_text(&mut line, &position);
        ui::live_line(
            font,
            (index as u32) % ui::LIVE_ROWS,
            line.as_str(),
            colour_of(&position),
        );
        ui::sub(index + 1, count);
        push_position(&mut out, index, &position);
    }
    // SAFETY: single thread; plain static.
    let skipped = unsafe { DEAD_SKIPPED };
    out.push(record(RADIUS_RECORD, count as u32, done as u32, GROUP));
    out.push(record(
        RADIUS_RECORD + 2,
        sectors & 0xFFFF,
        sectors >> 16,
        skipped,
    ));
    crate::run::cd_quiesce();
    out
}

fn push_position(out: &mut RadiusRecords, index: usize, position: &Position) {
    let base = POSITION_RECORD + RECORDS_PER_POSITION * index as u16;
    for (method, cell) in position.cells.iter().enumerate() {
        let id = base + 2 * method as u16;
        out.push(record(id, cell.delivered, cell.good, cell.centis));
        out.push(record(
            id + 1,
            cell.int5.min(0xFF) << 8 | cell.timeouts.min(0xFF),
            cell.retries.min(0xFF) << 8 | cell.error as u32,
            (cell.fold ^ (cell.fold >> 16)) & 0xFFFF,
        ));
    }
    let codes = position.cells[0].code.bits()
        | position.cells[1].code.bits() << 2
        | position.cells[2].code.bits() << 4;
    out.push(record(
        base + 6,
        position.lba & 0xFFFF,
        position.lba >> 16,
        codes,
    ));
}

/// The step's records: up to three meta, seven a position.
pub(crate) struct RadiusRecords {
    items: [TimingRecord; 3 + 7 * MAX_POSITIONS],
    len: usize,
}

impl RadiusRecords {
    const fn new() -> Self {
        Self {
            items: [TimingRecord::pending(); 3 + 7 * MAX_POSITIONS],
            len: 0,
        }
    }

    fn push(&mut self, record: TimingRecord) {
        if self.len < self.items.len() {
            self.items[self.len] = record;
            self.len += 1;
        }
    }

    pub(crate) fn as_slice(&self) -> &[TimingRecord] {
        &self.items[..self.len]
    }
}

fn print_dec(value: u32) {
    let mut buf = [0u8; 10];
    tty::print(crate::console_tests::number(&mut buf, value));
}

fn tty_cell(position: usize, method: usize, cell: &Cell) {
    tty::print("hardware-tests: radius pos ");
    print_dec(position as u32);
    tty::print([" sdk ", " raw2 ", " raw1 "][method]);
    tty::print(cell.code.text());
    tty::print(" delivered ");
    print_dec(cell.delivered);
    tty::print(" good ");
    print_dec(cell.good);
    tty::print(" int5 ");
    print_dec(cell.int5);
    tty::print(" timeouts ");
    print_dec(cell.timeouts);
    tty::print(" retries ");
    print_dec(cell.retries);
    tty::print(" cs ");
    print_dec(cell.centis);
    tty::print("\n");
}

// ------------------------------------------------------------------ pages

/// Capture pages this sweep adds to the run's picture set (0 when the
/// filler was not on the disc).
pub(crate) fn page_count() -> usize {
    // SAFETY: single thread; plain statics.
    let (found, len) = unsafe { (FILE_FOUND, TABLE_LEN) };
    if !found || len == 0 {
        0
    } else if len > 8 {
        2
    } else {
        1
    }
}

const PAGE_ROWS: usize = 13;
const HEAD: (u8, u8, u8) = (255, 232, 128);
const PLAIN: (u8, u8, u8) = (176, 190, 210);

/// Draw page `page` (0 or 1) of the table. Called every frame.
pub(crate) fn draw_page(font: &FontAtlas, page: usize) {
    // SAFETY: single thread; plain statics.
    let (table, len, skipped) = unsafe { (&*addr_of!(TABLE), TABLE_LEN, DEAD_SKIPPED) };
    let mut y = 6;
    let mut line = ui::Line::new();
    line.s("CD RADIUS SWEEP ").u(page as u32 + 1).s("/").u(page_count() as u32);
    font.draw_text(8, y, line.as_str(), HEAD);
    y += 11;
    if page == 0 {
        font.draw_text(8, y, "S SDK 2X   2 RAW 2X   1 RAW 1X  SEC", PLAIN);
        y += 9;
        font.draw_text(8, y, "OK  RT RETRIED  XX BAD DATA", PLAIN);
        y += 9;
        font.draw_text(8, y, "TO TIMEOUT  ER INT5 DRIVE ERROR", PLAIN);
        y += 12;
    }
    let (from, to) = if page == 0 {
        (0, len.min(PAGE_ROWS))
    } else {
        (PAGE_ROWS.min(len), len)
    };
    for position in &table[from..to] {
        let mut row = ui::Line::new();
        row_text(&mut row, position);
        font.draw_text(8, y, row.as_str(), colour_of(position));
        y += 9;
    }
    if page + 1 == page_count() {
        y += 6;
        summary(font, y, &table[..len], skipped);
    }
}

/// Totals under the last page: per method how many groups were clean, retried,
/// wrong or short, the error counts, and where the first fault was.
fn summary(font: &FontAtlas, mut y: i16, table: &[Position], skipped: u32) {
    for (method, tag) in ["SDK", "RAW2", "RAW1"].iter().enumerate() {
        let (mut ok, mut rt, mut xx, mut short) = (0u32, 0u32, 0u32, 0u32);
        let (mut int5, mut timeouts, mut retries) = (0u32, 0u32, 0u32);
        for position in table {
            let cell = &position.cells[method];
            match cell.code {
                Code::Ok => ok += 1,
                Code::Retried => rt += 1,
                Code::Wrong => xx += 1,
                Code::Timeout | Code::DriveError => short += 1,
                Code::None => {}
            }
            int5 += cell.int5;
            timeouts += cell.timeouts;
            retries += cell.retries;
        }
        let mut line = ui::Line::new();
        line.s(tag)
            .s(" OK ")
            .u(ok)
            .s(" RT ")
            .u(rt)
            .s(" XX ")
            .u(xx)
            .s(" SHORT ")
            .u(short);
        font.draw_text(8, y, line.as_str(), PLAIN);
        y += 9;
        let mut line = ui::Line::new();
        line.s("  INT5 ")
            .u(int5)
            .s(" TIMEOUTS ")
            .u(timeouts)
            .s(" RETRIES ")
            .u(retries);
        font.draw_text(8, y, line.as_str(), PLAIN);
        y += 9;
    }
    let first_fault = table.iter().find(|position| {
        position
            .cells
            .iter()
            .any(|cell| !matches!(cell.code, Code::Ok | Code::None))
    });
    let mut line = ui::Line::new();
    match first_fault {
        Some(position) => {
            line.s("FIRST FAULT AT ");
            clock_text(&mut line, position.lba);
            font.draw_text(8, y, line.as_str(), (255, 110, 110));
        }
        None => {
            line.s("EVERY GROUP READ CLEAN");
            font.draw_text(8, y, line.as_str(), (150, 230, 160));
        }
    }
    if skipped != 0 {
        y += 9;
        let mut line = ui::Line::new();
        line.u(skipped).s(" POSITIONS SKIPPED (DEAD OR SELECT)");
        font.draw_text(8, y, line.as_str(), (255, 216, 96));
    }
}
