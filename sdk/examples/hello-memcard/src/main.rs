// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-memcard` -- non-destructive, burnable memory-card hardware diagnostic.
//!
//! The test reads every one of the card's 1024 frames and hashes their contents
//! before enabling any write. It never formats a card and never overwrites an
//! existing file. Holding L1+R1 and pressing Cross performs a second full-card
//! identity scan, creates one reserved one-block test save with a native icon,
//! verifies it immediately, and leaves it to be found after a real power cycle.

#![no_std]
#![no_main]

extern crate psx_rt;

use hello_memcard_recovery::{exact_legacy_size_target, repair_exact_legacy_size};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_mc::{
    Block, Card, Entry, Error, HardwareCard, SaveIcon, Slot, TransportFault, TransportTrace,
    DATA_BLOCKS, FRAME_COUNT, FRAME_SIZE, MAX_NAME,
};
use psx_pad::{button, poll_port1, ButtonState};
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

// Deliberately unique and within the standard 20-byte BIOS filename limit.
const TEST_NAME: &str = "BESLES-00000PSXMC01";
const TEST_TITLE: &str = "PSOXIDE MC HARDWARE TEST";
const MAGIC: &[u8; 8] = b"PSXMCHW1";
const PAYLOAD_LEN: usize = 64;
/// The old diagnostic incorrectly stored container+payload bytes in the BIOS
/// directory size field: 16 + 64 = 0x50.
const LEGACY_DIRECTORY_SIZE: u32 = 0x50;
const BIOS_BLOCK_SIZE: u32 = 0x2000;

const GREEN: (u8, u8, u8) = (80, 230, 110);
const RED: (u8, u8, u8) = (240, 75, 75);
const AMBER: (u8, u8, u8) = (245, 190, 65);
const WHITE: (u8, u8, u8) = (225, 228, 235);
const GREY: (u8, u8, u8) = (145, 150, 165);

#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    Scan,
    PreWriteScan,
    Ready,
    Failed,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Prior {
    Absent,
    Valid(u32),
    Foreign,
    ReadError(Error),
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum WriteResult {
    NotRun,
    VerifyingCard,
    Pass(u32),
    RepairPass(u32),
    CardChanged,
    SafetyRefused(Error),
    WriteError(Error),
    ReadError(Error),
    Mismatch,
}

struct App {
    card: Card<HardwareCard>,
    phase: Phase,
    scan_frame: u16,
    scan_hash: u32,
    baseline_hash: u32,
    scan_bad: u16,
    first_error: Option<Error>,
    first_fault: TransportTrace,
    formatted: bool,
    free_blocks: u8,
    entries: [Entry; DATA_BLOCKS],
    entry_count: u8,
    prior: Prior,
    write_result: WriteResult,
    repair_expected: u32,
    last_trace: TransportTrace,
    page: u8,
}

impl App {
    fn new() -> Self {
        Self {
            card: Card::new(HardwareCard::new(Slot::One)),
            phase: Phase::Scan,
            scan_frame: 0,
            scan_hash: 0x811c_9dc5,
            baseline_hash: 0,
            scan_bad: 0,
            first_error: None,
            first_fault: empty_trace(),
            formatted: false,
            free_blocks: 0,
            entries: [empty_entry(); DATA_BLOCKS],
            entry_count: 0,
            prior: Prior::Absent,
            write_result: WriteResult::NotRun,
            repair_expected: 0,
            last_trace: empty_trace(),
            page: 0,
        }
    }

    /// Read two frames per video frame: fast enough to finish in about nine
    /// seconds at 60 Hz without hiding the progress screen.
    fn scan_step(&mut self) {
        if !matches!(self.phase, Phase::Scan | Phase::PreWriteScan) {
            return;
        }
        let scan_phase = self.phase;
        let mut count = 0;
        while count < 2 && (self.scan_frame as usize) < FRAME_COUNT {
            let mut frame = [0u8; FRAME_SIZE];
            let result = self.card.device().read_frame(self.scan_frame, &mut frame);
            self.last_trace = self.card.device().last_trace();
            match result {
                Ok(()) => {
                    self.scan_hash = hash_bytes(self.scan_hash, &frame);
                }
                Err(e) => {
                    self.scan_bad += 1;
                    if self.first_error.is_none() {
                        self.first_error = Some(e);
                        self.first_fault = self.last_trace;
                    }
                    // A low-level timeout on every remaining frame would take
                    // minutes and cannot yield a trustworthy filesystem test.
                    if self.last_trace.fault != TransportFault::None {
                        self.phase = Phase::Failed;
                        return;
                    }
                }
            }
            self.scan_frame += 1;
            count += 1;
        }

        if (self.scan_frame as usize) == FRAME_COUNT {
            if self.scan_bad == 0 {
                match scan_phase {
                    Phase::Scan => {
                        self.baseline_hash = self.scan_hash;
                        self.finish_scan();
                    }
                    Phase::PreWriteScan if self.scan_hash == self.baseline_hash => {
                        self.preflight_and_write()
                    }
                    Phase::PreWriteScan => {
                        self.write_result = WriteResult::CardChanged;
                        self.phase = Phase::Ready;
                    }
                    _ => {}
                }
            } else {
                self.phase = Phase::Failed;
            }
        }
    }

    fn finish_scan(&mut self) {
        match self.card.is_formatted() {
            Ok(true) => self.formatted = true,
            Ok(false) => {
                self.phase = Phase::Ready;
                return;
            }
            Err(e) => {
                self.fail(e);
                return;
            }
        }

        self.repair_expected = 0;
        if let Err(e) = self.card.validate_filesystem() {
            if e != Error::Corrupt {
                self.fail(e);
                return;
            }
            match exact_legacy_size_target(&mut self.card, TEST_NAME, LEGACY_DIRECTORY_SIZE) {
                Ok(Some(expected)) if expected == BIOS_BLOCK_SIZE => {
                    self.repair_expected = expected;
                }
                Ok(_) => {
                    self.fail(Error::Corrupt);
                    return;
                }
                Err(repair_error) => {
                    self.fail(repair_error);
                    return;
                }
            }
        }

        match self.card.free_blocks() {
            Ok(n) => self.free_blocks = n as u8,
            Err(e) => {
                self.fail(e);
                return;
            }
        }
        match self.card.list(&mut self.entries) {
            Ok(n) => self.entry_count = n as u8,
            Err(e) => {
                self.fail(e);
                return;
            }
        }

        let mut payload = [0u8; PAYLOAD_LEN];
        self.prior = match self.card.read(TEST_NAME, &mut payload) {
            Ok(n) => match validate_payload(&payload, n) {
                Some(generation) => Prior::Valid(generation),
                None => Prior::Foreign,
            },
            Err(Error::NotFound) => Prior::Absent,
            Err(e) => Prior::ReadError(e),
        };
        if self.repair_expected != 0 && !matches!(self.prior, Prior::Valid(_)) {
            self.fail(Error::Corrupt);
            return;
        }
        self.last_trace = self.card.device().last_trace();
        self.phase = Phase::Ready;
    }

    fn fail(&mut self, error: Error) {
        self.first_error = Some(error);
        self.first_fault = self.card.device().last_trace();
        self.last_trace = self.first_fault;
        self.phase = Phase::Failed;
    }

    fn write_enabled(&self) -> bool {
        self.phase == Phase::Ready
            && self.formatted
            && ((self.prior == Prior::Absent && self.free_blocks > 0)
                || (self.repair_expected == BIOS_BLOCK_SIZE
                    && matches!(self.prior, Prior::Valid(_))))
    }

    fn begin_write_test(&mut self) {
        if !self.write_enabled() {
            return;
        }
        self.phase = Phase::PreWriteScan;
        self.scan_frame = 0;
        self.scan_hash = 0x811c_9dc5;
        self.scan_bad = 0;
        self.first_error = None;
        self.first_fault = empty_trace();
        self.write_result = WriteResult::VerifyingCard;
    }

    fn preflight_and_write(&mut self) {
        if self.repair_expected != 0 {
            self.preflight_and_repair();
            return;
        }
        if let Err(e) = self.card.validate_filesystem() {
            self.write_result = WriteResult::SafetyRefused(e);
            self.phase = Phase::Ready;
            return;
        }
        let mut existing = [0u8; PAYLOAD_LEN];
        match self.card.read(TEST_NAME, &mut existing) {
            Err(Error::NotFound) => {}
            Ok(_) | Err(Error::BadContainer) => {
                self.prior = Prior::Foreign;
                self.write_result = WriteResult::SafetyRefused(Error::Exists);
                self.phase = Phase::Ready;
                return;
            }
            Err(e) => {
                self.write_result = WriteResult::SafetyRefused(e);
                self.phase = Phase::Ready;
                return;
            }
        }
        match self.card.free_blocks() {
            Ok(0) => {
                self.write_result = WriteResult::SafetyRefused(Error::NoSpace);
                self.phase = Phase::Ready;
                return;
            }
            Ok(_) => {}
            Err(e) => {
                self.write_result = WriteResult::SafetyRefused(e);
                self.phase = Phase::Ready;
                return;
            }
        }

        let generation = 1;
        let payload = make_payload(generation);
        let icon = make_test_icon();
        if let Err(e) = self
            .card
            .write_with_icon(TEST_NAME, TEST_TITLE, &payload, &icon)
        {
            self.write_result = WriteResult::WriteError(e);
            self.last_trace = self.card.device().last_trace();
            self.phase = Phase::Ready;
            return;
        }

        let mut readback = [0u8; PAYLOAD_LEN];
        match self.card.read(TEST_NAME, &mut readback) {
            Ok(n) if validate_payload(&readback, n) == Some(generation) => {
                self.write_result = WriteResult::Pass(generation);
                self.prior = Prior::Valid(generation);
            }
            Ok(_) => self.write_result = WriteResult::Mismatch,
            Err(e) => self.write_result = WriteResult::ReadError(e),
        }
        self.last_trace = self.card.device().last_trace();
        if self.write_result == WriteResult::Pass(generation) {
            if let Err(e) = self.card.validate_filesystem() {
                self.write_result = WriteResult::SafetyRefused(e);
            } else {
                self.free_blocks = self.card.free_blocks().unwrap_or(0) as u8;
                self.entry_count = self.card.list(&mut self.entries).unwrap_or(0) as u8;
            }
        }
        self.phase = Phase::Ready;
    }

    fn preflight_and_repair(&mut self) {
        let generation = match self.prior {
            Prior::Valid(generation) => generation,
            _ => {
                self.write_result = WriteResult::SafetyRefused(Error::Corrupt);
                self.phase = Phase::Ready;
                return;
            }
        };
        match exact_legacy_size_target(&mut self.card, TEST_NAME, LEGACY_DIRECTORY_SIZE) {
            Ok(Some(expected)) if expected == self.repair_expected => {}
            Ok(_) => {
                self.write_result = WriteResult::SafetyRefused(Error::Corrupt);
                self.phase = Phase::Ready;
                return;
            }
            Err(e) => {
                self.write_result = WriteResult::SafetyRefused(e);
                self.phase = Phase::Ready;
                return;
            }
        }

        // The reserved name alone is not authority to repair: verify that its
        // payload is exactly the diagnostic save observed before the second
        // unchanged-card scan.
        let mut payload = [0u8; PAYLOAD_LEN];
        match self.card.read(TEST_NAME, &mut payload) {
            Ok(n) if validate_payload(&payload, n) == Some(generation) => {}
            Ok(_) => {
                self.write_result = WriteResult::SafetyRefused(Error::BadContainer);
                self.phase = Phase::Ready;
                return;
            }
            Err(e) => {
                self.write_result = WriteResult::SafetyRefused(e);
                self.phase = Phase::Ready;
                return;
            }
        }

        match repair_exact_legacy_size(&mut self.card, TEST_NAME, LEGACY_DIRECTORY_SIZE) {
            Ok(expected) if expected == BIOS_BLOCK_SIZE => {}
            Ok(_) => {
                self.write_result = WriteResult::SafetyRefused(Error::Corrupt);
                self.phase = Phase::Ready;
                return;
            }
            Err(e) => {
                self.write_result = WriteResult::WriteError(e);
                self.last_trace = self.card.device().last_trace();
                self.phase = Phase::Ready;
                return;
            }
        }

        let mut readback = [0u8; PAYLOAD_LEN];
        self.write_result = match self.card.read(TEST_NAME, &mut readback) {
            Ok(n) if validate_payload(&readback, n) == Some(generation) => {
                WriteResult::RepairPass(generation)
            }
            Ok(_) => WriteResult::Mismatch,
            Err(e) => WriteResult::ReadError(e),
        };
        self.last_trace = self.card.device().last_trace();
        if self.write_result == WriteResult::RepairPass(generation) {
            if let Err(e) = self.card.validate_filesystem() {
                self.write_result = WriteResult::SafetyRefused(e);
            } else {
                self.repair_expected = 0;
                self.free_blocks = self.card.free_blocks().unwrap_or(0) as u8;
                self.entry_count = self.card.list(&mut self.entries).unwrap_or(0) as u8;
            }
        }
        self.phase = Phase::Ready;
    }
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    let mut app = App::new();
    let mut previous = ButtonState::NONE;

    loop {
        app.scan_step();

        // The card transaction is complete and /CS is high before polling the
        // pad; the two devices share SIO0 and are never accessed concurrently.
        let current = poll_port1().buttons;
        if pressed(current, previous, button::LEFT) {
            app.page = app.page.saturating_sub(1);
        }
        if pressed(current, previous, button::RIGHT) {
            app.page = (app.page + 1).min(2);
        }
        if pressed(current, previous, button::CROSS)
            && current.is_held(button::L1)
            && current.is_held(button::R1)
        {
            app.begin_write_test();
        }
        previous = current;

        fb.clear(9, 11, 18);
        match app.page {
            0 => draw_summary(&font, &app),
            1 => draw_files(&font, &app),
            _ => draw_transport(&font, &app),
        }
        draw_footer(&font, app.page);

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}

fn draw_summary(font: &FontAtlas, app: &App) {
    font.draw_text(8, 6, "PSOXIDE MEMORY CARD HW TEST", WHITE);
    font.draw_text(8, 20, "SLOT 1 - NEVER FORMATS THE CARD", AMBER);

    let mut y = 42;
    let mut b = Text::new();
    b.push("RAW SCAN ");
    b.dec(app.scan_frame as u32);
    b.push("/1024");
    line(
        font,
        y,
        b.as_str(),
        if app.phase == Phase::Failed {
            RED
        } else {
            WHITE
        },
    );
    y += 13;

    b.clear();
    b.push("READ ERRORS ");
    b.dec(app.scan_bad as u32);
    line(
        font,
        y,
        b.as_str(),
        if app.scan_bad == 0 { GREEN } else { RED },
    );
    y += 13;

    b.clear();
    b.push("CARD HASH ");
    b.hex32(app.scan_hash);
    line(font, y, b.as_str(), GREY);
    y += 13;

    match app.phase {
        Phase::Scan => line(font, y, "STATUS: READING EVERY FRAME...", AMBER),
        Phase::PreWriteScan => line(font, y, "STATUS: VERIFYING SAME CARD...", AMBER),
        Phase::Failed => {
            b.clear();
            b.push("STATUS: FAIL ");
            if let Some(e) = app.first_error {
                b.push(error_name(e));
            }
            line(font, y, b.as_str(), RED);
        }
        Phase::Ready if !app.formatted => line(font, y, "STATUS: UNFORMATTED - NO WRITE", RED),
        Phase::Ready if app.repair_expected != 0 => {
            line(font, y, "STATUS: DIRECTORY REPAIR NEEDED", AMBER)
        }
        Phase::Ready => line(font, y, "STATUS: FULL READ PASS", GREEN),
    }
    y += 16;

    if app.phase == Phase::Ready && app.formatted {
        b.clear();
        b.push("FILES ");
        b.dec(app.entry_count as u32);
        b.push("   FREE BLOCKS ");
        b.dec(app.free_blocks as u32);
        line(font, y, b.as_str(), WHITE);
        y += 14;

        match app.prior {
            Prior::Absent => line(font, y, "POWER-CYCLE SAVE: NOT CREATED", GREY),
            Prior::Valid(generation) if app.repair_expected != 0 => {
                b.clear();
                b.push("TEST SAVE VERIFIED GEN ");
                b.dec(generation);
                line(font, y, b.as_str(), GREEN);
            }
            Prior::Valid(generation) => {
                b.clear();
                b.push("POWER-CYCLE SAVE: PASS GEN ");
                b.dec(generation);
                line(font, y, b.as_str(), GREEN);
            }
            Prior::Foreign => line(font, y, "RESERVED NAME OCCUPIED - REFUSING", RED),
            Prior::ReadError(e) => {
                b.clear();
                b.push("TEST SAVE READ FAIL ");
                b.push(error_name(e));
                line(font, y, b.as_str(), RED);
            }
        }
        y += 14;

        match app.write_result {
            WriteResult::NotRun if app.repair_expected != 0 && app.write_enabled() => {
                line(font, y, "SIZE 0050 -> 2000 (ONE ENTRY)", AMBER);
                y += 14;
                line(font, y, "HOLD L1+R1, PRESS X TO REPAIR", AMBER)
            }
            WriteResult::NotRun if app.write_enabled() => {
                line(font, y, "HOLD L1+R1, PRESS X TO WRITE", AMBER)
            }
            WriteResult::NotRun if matches!(app.prior, Prior::Valid(_)) => {
                line(font, y, "COMPLETE - TEST SAVE NEVER OVERWRITTEN", GREEN)
            }
            WriteResult::NotRun => line(font, y, "WRITE LOCKED FOR CARD SAFETY", RED),
            WriteResult::VerifyingCard => line(font, y, "SECOND FULL SCAN IN PROGRESS", AMBER),
            WriteResult::Pass(generation) => {
                b.clear();
                b.push("WRITE+READBACK PASS GEN ");
                b.dec(generation);
                line(font, y, b.as_str(), GREEN);
                y += 14;
                line(font, y, "POWER OFF, REBOOT, EXPECT PASS", AMBER);
            }
            WriteResult::RepairPass(generation) => {
                b.clear();
                b.push("DIRECTORY REPAIR PASS GEN ");
                b.dec(generation);
                line(font, y, b.as_str(), GREEN);
                y += 14;
                line(font, y, "POWER OFF, CHECK SONY BIOS", AMBER);
            }
            WriteResult::CardChanged => line(font, y, "REFUSED: CARD CONTENTS CHANGED", RED),
            WriteResult::SafetyRefused(e) => {
                b.clear();
                b.push("SAFETY REFUSAL ");
                b.push(error_name(e));
                line(font, y, b.as_str(), RED);
            }
            WriteResult::WriteError(e) => {
                b.clear();
                b.push("WRITE FAIL ");
                b.push(error_name(e));
                line(font, y, b.as_str(), RED);
            }
            WriteResult::ReadError(e) => {
                b.clear();
                b.push("READBACK FAIL ");
                b.push(error_name(e));
                line(font, y, b.as_str(), RED);
            }
            WriteResult::Mismatch => line(font, y, "READBACK FAIL MISMATCH", RED),
        }
    }
}

fn draw_files(font: &FontAtlas, app: &App) {
    font.draw_text(8, 6, "CARD DIRECTORY (READ ONLY)", WHITE);
    if app.phase != Phase::Ready || !app.formatted {
        font.draw_text(8, 30, "AVAILABLE AFTER A CLEAN FULL SCAN", AMBER);
        return;
    }
    if app.entry_count == 0 {
        font.draw_text(8, 30, "NO SAVE FILES", GREY);
        return;
    }
    let mut y = 25;
    let mut i = 0usize;
    while i < app.entry_count as usize {
        let entry = app.entries[i];
        let mut b = Text::new();
        b.dec((i + 1) as u32);
        b.push(" ");
        b.push(entry.name());
        b.push(" [");
        b.dec(entry.blocks as u32);
        b.push("]");
        line(font, y, b.as_str(), WHITE);
        y += 12;
        i += 1;
    }
}

fn draw_transport(font: &FontAtlas, app: &App) {
    font.draw_text(8, 6, "LAST SIO0 TRANSACTION", WHITE);
    let trace = if app.phase == Phase::Failed {
        app.first_fault
    } else {
        app.last_trace
    };
    let mut b = Text::new();
    b.push("EXCHANGES ");
    b.dec(trace.exchanges as u32);
    b.push("  ACKS ");
    b.dec(trace.acknowledgements as u32);
    line(font, 28, b.as_str(), WHITE);

    b.clear();
    b.push("FAULT ");
    b.push(fault_name(trace.fault));
    if trace.fault != TransportFault::None {
        b.push(" @BYTE ");
        b.dec(trace.fault_exchange as u32);
    }
    line(
        font,
        43,
        b.as_str(),
        if trace.fault == TransportFault::None {
            GREEN
        } else {
            RED
        },
    );

    font.draw_text(8, 64, "RX PREFIX (HEX)", GREY);
    b.clear();
    let mut i = 0;
    while i < trace.response_prefix.len() {
        b.hex8(trace.response_prefix[i]);
        b.push(" ");
        i += 1;
    }
    line(font, 78, b.as_str(), WHITE);

    font.draw_text(8, 104, "EXPECTED READ PREFIX", GREY);
    font.draw_text(8, 118, "FF FLAG 5A 5D 00 MSB 5C 5D MSB LSB", WHITE);
    font.draw_text(8, 145, "REPORT: FAULT, BYTE, RX PREFIX,", GREY);
    font.draw_text(8, 158, "SCAN ERRORS AND CARD HASH.", GREY);
}

fn draw_footer(font: &FontAtlas, page: u8) {
    let label = match page {
        0 => "< SUMMARY  1/3  FILES >",
        1 => "< SUMMARY  2/3  SIO >",
        _ => "< FILES    3/3  SIO >",
    };
    font.draw_text(8, 222, label, GREY);
}

fn line(font: &FontAtlas, y: i16, text: &str, colour: (u8, u8, u8)) {
    font.draw_text(8, y, text, colour);
}

fn pressed(now: ButtonState, previous: ButtonState, mask: u16) -> bool {
    now.is_held(mask) && !previous.is_held(mask)
}

const fn empty_entry() -> Entry {
    Entry {
        name: [0; MAX_NAME + 1],
        name_len: 0,
        blocks: 0,
    }
}

const fn empty_trace() -> TransportTrace {
    TransportTrace {
        exchanges: 0,
        acknowledgements: 0,
        fault: TransportFault::None,
        fault_exchange: u16::MAX,
        response_prefix: [0xff; 10],
    }
}

fn make_test_icon() -> SaveIcon {
    let mut palette = [0u16; 16];
    palette[0] = 0x0000; // transparent
    palette[1] = 0x1084; // dark blue-grey fill
    palette[2] = 0x7fe0; // cyan outline
    palette[3] = 0x7fff; // white
    palette[4] = 0x23e8; // green confirmation mark

    let mut pixels = [0u8; FRAME_SIZE];
    let mut y = 1usize;
    while y < 15 {
        let mut x = 2usize;
        while x < 14 {
            let colour = if x == 2 || x == 13 || y == 1 || y == 14 {
                2
            } else {
                1
            };
            set_icon_pixel(&mut pixels, x, y, colour);
            x += 1;
        }
        y += 1;
    }
    // Memory-card label and notch.
    let mut x = 5;
    while x < 11 {
        set_icon_pixel(&mut pixels, x, 3, 3);
        x += 1;
    }
    set_icon_pixel(&mut pixels, 10, 1, 0);
    set_icon_pixel(&mut pixels, 11, 1, 0);
    // A compact check mark.
    set_icon_pixel(&mut pixels, 5, 9, 4);
    set_icon_pixel(&mut pixels, 6, 10, 4);
    set_icon_pixel(&mut pixels, 7, 11, 4);
    set_icon_pixel(&mut pixels, 8, 10, 4);
    set_icon_pixel(&mut pixels, 9, 9, 4);
    set_icon_pixel(&mut pixels, 10, 8, 4);
    set_icon_pixel(&mut pixels, 11, 7, 4);

    SaveIcon::new(palette, pixels)
}

fn set_icon_pixel(pixels: &mut [u8; FRAME_SIZE], x: usize, y: usize, colour: u8) {
    let at = (y * 16 + x) / 2;
    if x & 1 == 0 {
        pixels[at] = (pixels[at] & 0xf0) | (colour & 0x0f);
    } else {
        pixels[at] = (pixels[at] & 0x0f) | ((colour & 0x0f) << 4);
    }
}

fn make_payload(generation: u32) -> [u8; PAYLOAD_LEN] {
    let mut out = [0u8; PAYLOAD_LEN];
    out[..MAGIC.len()].copy_from_slice(MAGIC);
    out[8] = 1;
    out[9..13].copy_from_slice(&generation.to_le_bytes());
    let mut i = 13;
    while i < 60 {
        out[i] = (i as u8)
            .wrapping_mul(37)
            .wrapping_add(generation as u8)
            .rotate_left((generation & 7) as u32);
        i += 1;
    }
    let checksum = hash_bytes(0x811c_9dc5, &out[..60]);
    out[60..64].copy_from_slice(&checksum.to_le_bytes());
    out
}

fn validate_payload(payload: &[u8; PAYLOAD_LEN], len: usize) -> Option<u32> {
    if len != PAYLOAD_LEN || &payload[..8] != MAGIC || payload[8] != 1 {
        return None;
    }
    let generation = u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]);
    if make_payload(generation) == *payload {
        Some(generation)
    } else {
        None
    }
}

fn hash_bytes(mut hash: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

fn error_name(error: Error) -> &'static str {
    match error {
        Error::NoCard => "NO-CARD",
        Error::Protocol => "PROTOCOL",
        Error::BadChecksum => "CHECKSUM",
        Error::OutOfRange => "RANGE",
        Error::NotFormatted => "UNFORMATTED",
        Error::NotFound => "NOT-FOUND",
        Error::NoSpace => "NO-SPACE",
        Error::Exists => "EXISTS",
        Error::Corrupt => "CORRUPT",
        Error::BufferTooSmall => "BUFFER",
        Error::BadContainer => "CONTAINER",
        Error::Compression => "COMPRESS",
        Error::BadName => "BAD-NAME",
    }
}

fn fault_name(fault: TransportFault) -> &'static str {
    match fault {
        TransportFault::None => "NONE",
        TransportFault::TxTimeout => "TX-TIMEOUT",
        TransportFault::RxTimeout => "RX-TIMEOUT",
        TransportFault::AckTimeout => "ACK-TIMEOUT",
        TransportFault::AckReleaseTimeout => "ACK-RELEASE",
    }
}

struct Text {
    bytes: [u8; 64],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn push(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = byte;
                self.len += 1;
            }
        }
    }

    fn dec(&mut self, value: u32) {
        let mut digits = [0u8; 10];
        let mut n = value;
        let mut len = 0;
        loop {
            digits[len] = b'0' + (n % 10) as u8;
            len += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        while len > 0 {
            len -= 1;
            self.push_byte(digits[len]);
        }
    }

    fn hex8(&mut self, value: u8) {
        self.push_byte(hex(value >> 4));
        self.push_byte(hex(value & 15));
    }

    fn hex32(&mut self, value: u32) {
        let mut shift = 28;
        loop {
            self.push_byte(hex(((value >> shift) & 15) as u8));
            if shift == 0 {
                break;
            }
            shift -= 4;
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn as_str(&self) -> &str {
        // SAFETY: every append path emits ASCII.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

fn hex(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'A' + value - 10
    }
}
