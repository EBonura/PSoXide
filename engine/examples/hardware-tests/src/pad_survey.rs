// SPDX-License-Identifier: GPL-2.0-or-later
//! CONTROLLER SURVEY: what each controller does on the wire, and what the shared SDK pad path
//! (`psx_pad::Controllers`) makes of it, with nothing waiting for a press.
//!
//! The mode blocks the frame loop like the linear run and never returns: it surveys the pad in a
//! port, shows the record's QR pages, and when a controller is unplugged and another plugged in
//! it starts the next numbered record. It is reached from the menu or by itself after
//! [`IDLE_ENTRY_FRAMES`] frames of no input at the menu, and every step advances on a timer.
//!
//! One record per controller (`pad_record.rs` is the format), in this order:
//!
//! 1. RAW SURVEY. Our own transactions on the SIO0 registers, not the SDK's pad driver: select,
//!    a measured setup time, then per byte a write, the reply, and the wait for `/ACK` with a
//!    40,000-cycle limit (23 times the 1,700 cycles a pad takes). Every byte sent and received,
//!    the ticks from the write to the `/ACK` rising and the ticks it stays up, missing `/ACK`s
//!    and timeouts, go in the record. Timer: root counter 0, the system clock, 33,868,800 Hz,
//!    16 bits. The survey is repeated at four spacings: MIN, SHORT, MEDIUM, LONG.
//! 2. GUIDED WINDOWS. Eight windows of a few seconds each, timed with a countdown. Every frame
//!    in a window reads the pad twice: through `Controllers` (the SDK path, as the launcher and
//!    the games poll it, with a watching transport that keeps the wire bytes) and with a raw
//!    transaction on a fresh select. The window keeps both, side by side.
//! 3. SOAKS. Hundreds of consecutive `Controllers` polls as the launcher loops them: nothing
//!    else running; then with CD XA, an SPU stream and display flips running; then with the raw
//!    poll in the same frame as the control.
//! 4. HAND-OVER. The launcher's loop reads the pad, `psx_rt::entry::set_entry_state` (what the
//!    chain loader's quiesce calls) resets SIO0, the timers and the interrupt mask, and a new
//!    `Controllers` makes the game's first poll, which runs a second handshake on a pad the
//!    launcher already put in analog mode and locked. Its next polls are kept.
//!
//! The wire is read with CPU interrupts masked for the length of one raw transaction, as the
//! SDK masks its configuration packets: a handler that runs over the `/ACK` pulse hides it.

use crate::pad_record::{
    self as rec, Axis, Handover, Header, PollView, Soak, Txn, Window, Writer, TXN_MAX,
};
use crate::payload::{append, base64_encode, crc32, draw_qr, QR_QUIET};
use crate::report::hex4;
use crate::{hex2, hex8};
use crate::ui::{self, Line};
use crate::xa_loop;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_gpu::prim::QuadFlat;
use psx_gpu::Gpu;
use psx_hw::sio::sio0;
use psx_io::controller_port::{Port, Transport};
use psx_io::cd::xa::Player;
use psx_io::gpu as gpu_io;
use psx_io::irq;
use psx_io::periph::ControllerPort;
use psx_pad::{AnalogRequirement, Controllers, PadMode, PadState};
use psx_rt::{interrupts, tty};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

/// Menu frames with no input before the mode starts by itself (20 seconds at 60 Hz).
pub(crate) const IDLE_ENTRY_FRAMES: u32 = 20 * 60;

/// SDK revision the pad path of this build comes from.
const SDK_REV: [u8; 8] = *b"9bb3045b";
/// System clock, the rate of root counter 0.
const TIMER_HZ: u32 = 33_868_800;
/// Ticks to wait for an `/ACK` after a byte before calling it missing.
const ACK_TIMEOUT: u16 = 40_000;
/// Frames of the survey's wait for a pad before it records "nothing answered".
const NO_PAD_FRAMES: u32 = 12 * 60;
/// Frames a QR page stays up.
const PAGE_FRAMES: u32 = 150;
/// Largest record, bytes.
const RECORD_MAX: usize = 11 * 1024;
/// Records kept for the page cycle.
const RECORDS: usize = 3;
/// Most pages a record can need.
const QR_VERSION_MAX: Version = Version::new(20);
const QR_VERSION_MIN: Version = Version::new(10);
const QR_SIZE: usize = 97;
const QR_AREA: i16 = 200;
const SDK_POLLS: usize = 300;
const GAME_POLLS: usize = 120;
/// Pad presence: polls in a row that must agree before a change is believed.
const PRESENCE_HOLD: u8 = 6;

const INK: (u8, u8, u8) = (232, 236, 244);
const DIM: (u8, u8, u8) = (150, 170, 200);
const GOLD: (u8, u8, u8) = (255, 232, 128);
const GOOD: (u8, u8, u8) = (96, 240, 128);

/// One spacing of the survey: the delay after the select before the first byte, the delay after
/// each `/ACK` before the next byte, and the delay after the transaction before the next
/// command, all in system-clock ticks.
#[derive(Clone, Copy)]
struct Profile {
    setup: u16,
    gap: u16,
    between: u32,
}

/// MIN, SHORT, MEDIUM (the SDK's own setup time), LONG (one video frame between commands).
const PROFILES: [Profile; 4] = [
    Profile { setup: 0, gap: 0, between: 0 },
    Profile { setup: 2_048, gap: 0, between: 4_096 },
    Profile { setup: 7_168, gap: 400, between: 33_000 },
    Profile { setup: 24_576, gap: 3_000, between: 560_000 },
];
/// The spacing of the raw polls that ride beside the SDK's: the SDK's setup time, no gaps.
const POLL_PROFILE: Profile = Profile { setup: 7_168, gap: 0, between: 0 };

const POLL_TX: [u8; 21] = [
    0x01, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn cfg(cmd: u8, a: u8, b: u8) -> [u8; 9] {
    [0x01, cmd, 0x00, a, b, 0x00, 0x00, 0x00, 0x00]
}

/// Spin `cycles` system-clock ticks.
fn wait_ticks(bus: &mut ControllerPort, cycles: u32) {
    let mut left = cycles;
    while left > 0 {
        let chunk = left.min(30_000) as u16;
        let start = bus.clock();
        while bus.clock().wrapping_sub(start) < chunk {}
        left -= u32::from(chunk);
    }
}

/// Reply bytes a pad of this ID sends in all, address byte included: three, and two for every
/// halfword the low nibble of the ID counts.
fn reply_len(id: u8) -> usize {
    3 + 2 * usize::from(id & 0x0F).max(1)
}

/// One transaction on the registers: select `port`, wait `profile.setup`, write each byte of `tx`
/// and wait for its `/ACK` by watching `STAT`, measured on root counter 0. The pad's own ID
/// byte, the second reply, sets how many bytes the exchange has. Nothing here is the SDK's
/// exchange: the waits, the limits and what is recorded are this function's.
#[inline(never)]
fn transact(
    bus: &mut ControllerPort,
    port: Port,
    tx: &[u8],
    profile: Profile,
    step: u8,
    prof: u8,
    seq: u8,
) -> Txn {
    let mut t = Txn::EMPTY;
    t.step = step;
    t.prof = prof;
    t.seq = seq;
    let ints = bus.mask_interrupts();
    bus.select(port);
    let opened = bus.clock();
    while bus.clock().wrapping_sub(opened) < profile.setup {}
    bus.drain_receive();
    if !bus.wait_status_clear(sio0::stat::DSR_LEVEL, 4096) {
        t.flags |= 1;
    }
    let mut total = tx.len().min(TXN_MAX);
    let mut i = 0;
    while i < total {
        t.tx[i] = tx[i];
        t.sent = (i + 1) as u8;
        if !bus.wait_status_set(sio0::stat::TX_READY, 4096) {
            t.flags |= 4;
            break;
        }
        let t0 = bus.clock();
        bus.transmit(tx[i]);
        // The reply shifts in 1,084 ticks after the write; the `/ACK` pulse comes later (about
        // 1,600), so it is watched for here as well and is never missed by waiting for the reply.
        let mut rise: Option<u16> = None;
        let mut latch_only = false;
        let mut got = false;
        loop {
            let status = bus.status();
            let now = bus.clock().wrapping_sub(t0);
            if status & sio0::stat::DSR_LEVEL != 0 && rise.is_none() {
                rise = Some(now);
            }
            if status & sio0::stat::RX_NOT_EMPTY != 0 {
                got = true;
                break;
            }
            if now > ACK_TIMEOUT {
                break;
            }
        }
        if !got {
            t.flags |= 4;
            break;
        }
        t.rx[i] = bus.receive();
        t.got = (i + 1) as u8;
        if i == 1 {
            let id = t.rx[1];
            total = if id == 0xFF { 2 } else { reply_len(id).min(total) };
        }
        let last = i + 1 >= total;
        if !last {
            // Wait for the `/ACK` if it has not shown, as level or as the latched bit.
            while rise.is_none() {
                let status = bus.status();
                let now = bus.clock().wrapping_sub(t0);
                if status & sio0::stat::DSR_LEVEL != 0 {
                    rise = Some(now);
                } else if status & sio0::stat::IRQ != 0 {
                    rise = Some(now);
                    latch_only = true;
                } else if now > ACK_TIMEOUT {
                    break;
                }
            }
            match rise {
                None => {
                    t.flags |= 2;
                    break;
                }
                Some(r) => {
                    t.rise[i] = r;
                    if latch_only {
                        t.width[i] = 0xFF;
                    } else {
                        let up = bus.clock();
                        while bus.status() & sio0::stat::DSR_LEVEL != 0
                            && bus.clock().wrapping_sub(up) < 255
                        {}
                        t.width[i] = bus.clock().wrapping_sub(up).min(254) as u8;
                    }
                    if profile.gap != 0 {
                        let gap = u32::from(profile.gap);
                        wait_ticks(bus, gap);
                    }
                }
            }
        }
        i += 1;
    }
    let ok = t.flags & 0x07 == 0;
    bus.finish(ok);
    bus.restore_interrupts(ints);
    t
}

/// Whether a transaction read as a complete reply from a pad: the magic 0x5A and every byte.
fn txn_ok(t: &Txn) -> bool {
    t.got >= 3 && t.got == t.sent && t.flags & 0x07 == 0 && t.rx[2] == 0x5A
}

/// Active-high buttons from the two wire bytes (3 and 4 of the reply).
fn wire_buttons(rx: &[u8; TXN_MAX]) -> u16 {
    !(u16::from(rx[3]) | (u16::from(rx[4]) << 8))
}

fn mode_code(mode: PadMode) -> u8 {
    match mode {
        PadMode::Disconnected => 0,
        PadMode::Digital => 1,
        PadMode::Analog => 2,
        PadMode::Negcon => 3,
        PadMode::Config => 4,
        PadMode::Unknown => 5,
    }
}

/// The real port under `Controllers`, watching it: it keeps the reply bytes of the last poll
/// transaction (command 0x42), the `/ACK` timing of each byte, and counts poll and
/// configuration transactions. It reads the system clock only when `STAT` shows an `/ACK`
/// signal or the line released, never inside the SDK's wait loops, so it adds no time to them.
struct Watch {
    port: ControllerPort,
    position: usize,
    command: u8,
    rx: [u8; 9],
    n: u8,
    rise: [u16; 9],
    width: [u8; 9],
    up_at: u16,
    wrote_at: u16,
    clear_pending: bool,
    irq_after_clear: bool,
    snap: [u8; 9],
    snap_n: u8,
    snap_ok: bool,
    snap_wire: rec::Wire,
    snap_irq: bool,
    polls: u32,
    configs: u32,
}

impl Watch {
    fn new() -> Self {
        Self {
            // SAFETY: a token is a logic guard; nothing else drives SIO0 while the survey runs.
            port: unsafe { ControllerPort::steal() },
            position: 0,
            command: 0,
            rx: [0; 9],
            n: 0,
            rise: [0xFFFF; 9],
            width: [0; 9],
            up_at: 0,
            wrote_at: 0,
            clear_pending: false,
            irq_after_clear: false,
            snap: [0; 9],
            snap_n: 0,
            snap_ok: false,
            snap_wire: rec::Wire::NONE,
            snap_irq: false,
            polls: 0,
            configs: 0,
        }
    }

    /// Index of the byte last written.
    fn current(&self) -> Option<usize> {
        self.position.checked_sub(1).filter(|&i| i < 9)
    }
}

impl Transport for Watch {
    fn status(&mut self) -> u32 {
        let status = self.port.status();
        if self.clear_pending {
            self.clear_pending = false;
            if status & sio0::stat::IRQ != 0 {
                self.irq_after_clear = true;
            }
        }
        if let Some(i) = self.current() {
            if status & (sio0::stat::DSR_LEVEL | sio0::stat::IRQ) != 0 && self.rise[i] == 0xFFFF {
                let now = self.port.clock();
                self.rise[i] = now.wrapping_sub(self.wrote_at);
                if status & sio0::stat::DSR_LEVEL != 0 {
                    self.up_at = now;
                    self.width[i] = 0;
                } else {
                    self.width[i] = 0xFF;
                }
            } else if status & sio0::stat::DSR_LEVEL == 0
                && self.rise[i] != 0xFFFF
                && self.width[i] == 0
            {
                self.width[i] = self.port.clock().wrapping_sub(self.up_at).clamp(1, 254) as u8;
            }
        }
        status
    }
    fn receive(&mut self) -> u8 {
        let byte = self.port.receive();
        if let Some(at) = self.position.checked_sub(1) {
            if at < 9 {
                self.rx[at] = byte;
                self.n = (at + 1) as u8;
            }
        }
        byte
    }
    fn transmit(&mut self, byte: u8) {
        if self.position == 1 {
            self.command = byte;
        }
        self.position += 1;
        self.wrote_at = self.port.clock();
        self.port.transmit(byte);
    }
    fn set_mode(&mut self, value: u16) {
        self.port.set_mode(value);
    }
    fn set_baud(&mut self, value: u16) {
        self.port.set_baud(value);
    }
    fn set_control(&mut self, value: u16) {
        if value & sio0::ctrl::ACK != 0 {
            self.clear_pending = true;
        }
        self.port.set_control(value);
    }
    fn control(&mut self) -> u16 {
        self.port.control()
    }
    fn clock(&mut self) -> u16 {
        self.port.clock()
    }
    fn finish(&mut self, succeeded: bool) {
        if self.command == 0x42 {
            self.snap = self.rx;
            self.snap_n = self.n;
            self.snap_ok = succeeded;
            self.snap_wire = rec::Wire { rise: self.rise, width: self.width };
            self.snap_irq = self.irq_after_clear;
            self.polls += 1;
        } else if self.position > 0 {
            self.configs += 1;
        }
        self.position = 0;
        self.command = 0;
        self.n = 0;
        self.rx = [0; 9];
        self.rise = [0xFFFF; 9];
        self.width = [0; 9];
        self.irq_after_clear = false;
        self.port.finish(succeeded);
    }
    fn mask_interrupts(&mut self) -> bool {
        self.port.mask_interrupts()
    }
    fn restore_interrupts(&mut self, were_enabled: bool) {
        self.port.restore_interrupts(were_enabled);
    }
}

/// What one `Controllers::poll` did, from the SDK's report and the wire.
struct SdkRead {
    view: PollView,
    wire: rec::Wire,
    failed: bool,
    stale: bool,
    state: PadState,
}

/// Poll `pads` once through a watching transport and describe it.
#[inline(never)]
fn sdk_poll<const A: bool>(pads: &mut Controllers<A>, watch: &mut Watch, port: Port) -> SdkRead {
    let before = watch.polls;
    let state = *pads.poll(watch, port);
    let made = watch.polls != before;
    let n = if made { watch.snap_n } else { 0 };
    let rx = if made { watch.snap } else { [0; 9] };
    let wire = if made { watch.snap_wire } else { rec::Wire::NONE };
    let want = reply_len(rx[1]).min(9);
    let class = if !made {
        4
    } else if n < 2 || rx[1] == 0xFF {
        3
    } else if n >= 3 && rx[2] != 0x5A {
        2
    } else if !watch.snap_ok || usize::from(n) < want {
        1
    } else {
        0
    };
    let acked = wire.rise.iter().filter(|&&r| r != 0xFFFF).count() as u8;
    let early = wire.rise.iter().position(|&r| r < 1_000).map_or(0, |i| i as u8 + 1);
    let analog = pads.analog(port) == AnalogRequirement::Analog;
    let view = PollView {
        n,
        rx,
        mode: mode_code(state.mode),
        flags: u8::from(state.is_connected()) | (u8::from(analog) << 1),
        buttons: state.buttons.bits(),
        sticks: [
            state.sticks.right_x,
            state.sticks.right_y,
            state.sticks.left_x,
            state.sticks.left_y,
        ],
        class,
        acked,
        early,
        irq_after_clear: made && watch.snap_irq,
    };
    SdkRead { view, wire, failed: class != 0, stale: class != 0 && state.mode == PadMode::Analog, state }
}

// --------------------------------------------------------------------------- screen

/// Fixed-size lines of the on-screen log.
const LOG_LINES: usize = 6;

struct Screen {
    font: FontAtlas,
    log: [[u8; 40]; LOG_LINES],
    log_n: usize,
    record_no: u8,
    port: u8,
}

/// Run `draw` once into each of the two 240-line buffers, whichever is on show.
#[inline(never)]
fn both(draw: &mut dyn FnMut()) {
    probe_gpu!(gpu);
    gpu_io::wait_command_ready();
    for base in [0i16, 240] {
        // Clipped to this buffer: a line that runs past the right edge must not land on the
        // font atlas that sits at x 320.
        gpu.set_draw_area((0, base as u16), (319, base as u16 + 239));
        gpu.set_draw_offset((0, base));
        draw();
        gpu_io::wait_command_ready();
    }
    gpu.set_draw_offset((0, 0));
}

#[inline(never)]
fn rect(x: i16, y: i16, w: u16, h: u16, c: (u8, u8, u8)) {
    probe_gpu!(gpu);
    gpu.draw(&QuadFlat::rect((x, y), (w, h), c));
}

impl Screen {
    fn new() -> Self {
        Self { font: ui::upload_font(), log: [[b' '; 40]; LOG_LINES], log_n: 0, record_no: 0, port: 0 }
    }

    fn text(&self, x: i16, y: i16, s: &str, c: (u8, u8, u8)) {
        self.font.draw_text(x, y, s, c);
    }

    /// Clear and draw the header of a survey screen.
    fn begin(&self, title: &str) {
        both(&mut || {
            rect(0, 0, 320, 240, (6, 8, 18));
            let mut l = L::new();
            l.s("PAD SURVEY REC ").u(u32::from(self.record_no)).s(" PORT ").u(u32::from(self.port) + 1);
            self.text(4, 2, l.as_str(), INK);
            self.text(320 - 4 - crate::SUITE_VERSION.len() as i16 * 8, 2, crate::SUITE_VERSION, DIM);
            self.text(4, 14, title, GOLD);
        });
        self.draw_log();
    }

    fn draw_log(&self) {
        both(&mut || {
            rect(0, 168, 320, 72, (10, 14, 28));
            let mut y = 170;
            for i in 0..self.log_n.min(LOG_LINES) {
                let s = core::str::from_utf8(&self.log[i]).unwrap_or("");
                self.text(2, y, s.trim_end(), INK);
                y += 11;
            }
        });
    }

    /// Add a verdict line to the log, keeping the newest.
    fn say(&mut self, s: &str) {
        tty::println(s);
        if self.log_n == LOG_LINES {
            for i in 1..LOG_LINES {
                self.log[i - 1] = self.log[i];
            }
            self.log_n -= 1;
        }
        let mut line = [b' '; 40];
        for (d, b) in line.iter_mut().zip(s.bytes()) {
            *d = b;
        }
        self.log[self.log_n] = line;
        self.log_n += 1;
        self.draw_log();
    }

    /// Redraw the live area (rows 28 to 166) with `lines`.
    fn live(&self, lines: &[(&str, (u8, u8, u8))]) {
        present_scene(&mut || {
            rect(0, 26, 320, 140, (6, 8, 18));
            let mut y = 28;
            for (s, c) in lines {
                // An empty string is skipped: it is not a line, and the font draw of one leaves
                // the GPU waiting on a command.
                if !s.is_empty() {
                    self.text(4, y, s, *c);
                    y += 11;
                }
            }
        });
    }
}

fn hex_bytes(l: &mut L, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            l.s(" ");
        }
        l.hex(u32::from(*b), 2);
    }
}

/// Wait one video frame.
fn frame() {
    // The stop panel's watchdog counts GP1 writes and CD commands, which a survey that only
    // polls pads makes none of for minutes.
    crate::bounds::heartbeat();
    let _ = interrupts::try_wait_vblank(4_000_000);
}

// --------------------------------------------------------------------------- the survey

/// What the raw survey learned, for the on-screen verdict.
#[derive(Default)]
struct SurveyResult {
    /// Some transaction saw the address byte acknowledged, so something is in the port.
    any_ack: bool,
    answered: bool,
    first_id: u8,
    ok: [u16; 4],
    total: [u16; 4],
    last_id: u8,
}

fn survey_step(
    bus: &mut ControllerPort,
    port: Port,
    w: &mut Writer,
    res: &mut SurveyResult,
    tx: &[u8],
    step: u8,
    p: usize,
    seq: u8,
) -> Txn {
    let t = transact(bus, port, tx, PROFILES[p], step, p as u8, seq);
    rec::put_txn(w, &t);
    res.total[p] += 1;
    res.any_ack |= t.sent >= 1 && t.rise[0] != 0xFFFF;
    if txn_ok(&t) {
        res.ok[p] += 1;
    }
    if step == 1 && p == 0 {
        res.first_id = if t.got >= 2 { t.rx[1] } else { 0xFF };
        res.answered = txn_ok(&t);
    }
    if t.got >= 2 {
        res.last_id = t.rx[1];
    }
    wait_ticks(bus, PROFILES[p].between);
    t
}

/// The whole raw survey on one port, every profile, into the record.
#[inline(never)]
fn run_survey(scr: &mut Screen, bus: &mut ControllerPort, port: Port, w: &mut Writer) -> SurveyResult {
    let mut res = SurveyResult::default();
    let sec = w.begin_section(rec::TAG_TXN);
    for p in 0..PROFILES.len() {
        let mut l = L::new();
        l.s("RAW SURVEY, SPACING ").s(rec::PROFILE_NAMES[p]);
        scr.begin(l.as_str());
        let mut l = L::new();
        l.s("SETUP ").u(u32::from(PROFILES[p].setup)).s(" GAP ").u(u32::from(PROFILES[p].gap));
        l.s(" BETWEEN ").u(PROFILES[p].between);
        let mut l2 = L::new();
        l2.s("TICKS OF THE SYSTEM CLOCK 33868800 HZ");
        scr.live(&[
            (l.as_str(), INK),
            (l2.as_str(), DIM),
            ("EVERY BYTE AND ITS ACK IS RECORDED.", DIM),
            ("NOTHING TO PRESS.", GOOD),
        ]);
        let full = p != 0;
        let _ = survey_step(bus, port, w, &mut res, &POLL_TX, 1, p, 0);
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x01, 0x00), 2, p, 0);
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x45, 0, 0), 3, p, 0);
        if full {
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x46, 0x00, 0), 4, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x46, 0x01, 0), 5, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x47, 0x00, 0), 6, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x4C, 0x00, 0), 7, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x4C, 0x01, 0), 8, p, 0);
        }
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x44, 0x01, 0x03), 9, p, 0);
        let _ = survey_step(bus, port, w, &mut res, &POLL_TX, 10, p, 0);
        if full {
            let motors = [0x01, 0x4D, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF];
            let _ = survey_step(bus, port, w, &mut res, &motors, 11, p, 0);
        }
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x00, 0x00), 12, p, 0);
        let polls = if full { 4 } else { 3 };
        for k in 0..polls {
            let _ = survey_step(bus, port, w, &mut res, &POLL_TX, 13, p, k);
        }
        if full {
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x01, 0x00), 14, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x44, 0x00, 0x00), 15, p, 0);
            let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x00, 0x00), 16, p, 0);
            for k in 0..3 {
                let _ = survey_step(bus, port, w, &mut res, &POLL_TX, 17, p, k);
            }
        }
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x01, 0x00), 18, p, 0);
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x44, 0x01, 0x03), 19, p, 0);
        let _ = survey_step(bus, port, w, &mut res, &cfg(0x43, 0x00, 0x00), 20, p, 0);
        for k in 0..if full { 3 } else { 2 } {
            let _ = survey_step(bus, port, w, &mut res, &POLL_TX, 21, p, k);
        }
    }
    w.end_section(sec);
    let mut l = L::new();
    l.s("SURVEY OK/ALL MIN ").u(u32::from(res.ok[0])).s("/").u(u32::from(res.total[0]));
    l.s(" SHORT ").u(u32::from(res.ok[1])).s("/").u(u32::from(res.total[1]));
    scr.say(l.as_str());
    let mut l = L::new();
    l.s("MEDIUM ").u(u32::from(res.ok[2])).s("/").u(u32::from(res.total[2]));
    l.s(" LONG ").u(u32::from(res.ok[3])).s("/").u(u32::from(res.total[3]));
    scr.say(l.as_str());
    let mut l = L::new();
    l.s("ID AS POWERED ").hex(u32::from(res.first_id), 2).s(", AT THE END ").hex(u32::from(res.last_id), 2);
    scr.say(l.as_str());
    res
}

// --------------------------------------------------------------------------- windows

struct Prompt {
    name: &'static str,
    ask: &'static str,
    frames: u16,
}

const PROMPTS: [Prompt; 8] = [
    Prompt { name: "HANDS OFF", ask: "LET GO OF THE CONTROLLER", frames: 240 },
    Prompt { name: "HOLD X", ask: "PRESS AND HOLD X", frames: 300 },
    Prompt { name: "D-PAD", ask: "PRESS EACH D-PAD DIRECTION", frames: 360 },
    Prompt { name: "LEFT STICK", ask: "MOVE THE LEFT STICK IN A CIRCLE", frames: 360 },
    Prompt { name: "RIGHT STICK", ask: "MOVE THE RIGHT STICK IN A CIRCLE", frames: 360 },
    Prompt { name: "L3 R3", ask: "PRESS L3 AND R3 (CLICK THE STICKS)", frames: 300 },
    Prompt { name: "ANALOG BUTTON", ask: "PRESS THE ANALOG BUTTON NOW", frames: 300 },
    Prompt { name: "OTHER BUTTONS", ask: "PRESS ALL THE OTHER BUTTONS", frames: 360 },
];

fn spread(min: u8, max: u8) -> u8 {
    max.saturating_sub(min)
}

#[inline(never)]
fn run_windows(
    scr: &mut Screen,
    bus: &mut ControllerPort,
    port: Port,
    pads: &mut Controllers,
    watch: &mut Watch,
    w: &mut Writer,
) {
    let sec = w.begin_section(rec::TAG_WINDOW);
    for (phase, prompt) in PROMPTS.iter().enumerate() {
        let mut l = L::new();
        l.s("WINDOW ").u(phase as u32 + 1).s("/8 ").s(prompt.name);
        scr.begin(l.as_str());
        let mut win = Window::EMPTY;
        win.phase = phase as u8;
        win.frames_planned = prompt.frames;
        let at = [
            u32::from(prompt.frames) / 12,
            u32::from(prompt.frames) / 3,
            u32::from(prompt.frames) * 7 / 12,
            u32::from(prompt.frames) * 5 / 6,
        ];
        let mut frames = 0u32;
        while frames < u32::from(prompt.frames) {
            frame();
            let sdk = sdk_poll(pads, watch, port);
            let raw = transact(bus, port, &POLL_TX, POLL_PROFILE, 1, 0, 0);
            frames += 1;
            win.frames_run = frames as u16;
            // Raw side.
            let raw_ok = txn_ok(&raw);
            if raw_ok {
                win.raw_ok += 1;
                rec::push_id(&mut win.raw_ids, &mut win.raw_id_n, raw.rx[1]);
                if raw.got >= 5 {
                    let b = wire_buttons(&raw.rx);
                    win.raw_or |= b;
                    win.raw_and &= b;
                }
                if raw.got >= 9 {
                    for a in 0..4 {
                        let v = raw.rx[5 + a];
                        let ax: &mut Axis = &mut win.raw_axis[a];
                        ax.min = ax.min.min(v);
                        ax.max = ax.max.max(v);
                        for (s, moment) in at.iter().enumerate() {
                            if frames == *moment {
                                ax.samples[s] = v;
                            }
                        }
                    }
                }
            } else {
                win.raw_bad += 1;
            }
            // SDK side.
            if sdk.failed {
                win.sdk_bad += 1;
                if sdk.stale {
                    win.sdk_stale += 1;
                }
            } else {
                win.sdk_ok += 1;
            }
            let v = sdk.view;
            win.sdk_conn += u16::from(v.flags & 1);
            win.sdk_analog += u16::from(sdk.state.is_analog());
            rec::push_mode(&mut win.sdk_modes, &mut win.sdk_mode_n, v.mode, v.flags & 2 != 0);
            win.sdk_or |= v.buttons;
            win.sdk_and &= v.buttons;
            for a in 0..4 {
                win.sdk_min[a] = win.sdk_min[a].min(v.sticks[a]);
                win.sdk_max[a] = win.sdk_max[a].max(v.sticks[a]);
            }
            // Live bytes, every frame.
            let mut b = L::new();
            b.s("TIME LEFT ").u(u32::from(prompt.frames).saturating_sub(frames) / 60 + 1).s(" S  WINDOW ").u(phase as u32 + 1).s("/8");
            let mut x = L::new();
            x.s("RAW OK ").u(u32::from(win.raw_ok)).s(" BAD ").u(u32::from(win.raw_bad)).s(" SDK OK ").u(u32::from(win.sdk_ok));
            x.s(" BAD ").u(u32::from(win.sdk_bad));
            live_sdk(scr, prompt.ask, b.as_str(), &v, Some(&raw), x.as_str());
        }
        rec::put_window(w, &win);
        // The plain verdict for this window.
        let mut l = L::new();
        l.s(SHORT_NAMES[phase]).s(" BTN R").hex(u32::from(win.raw_or), 4).s(" S").hex(u32::from(win.sdk_or), 4);
        let rs = (0..4).map(|a| spread(win.raw_axis[a].min, win.raw_axis[a].max)).max().unwrap_or(0);
        let ss = (0..4).map(|a| spread(win.sdk_min[a], win.sdk_max[a])).max().unwrap_or(0);
        l.s(" STK ").hex(u32::from(rs), 2).s("/").hex(u32::from(ss), 2);
        if rs >= 0x40 && ss < 0x20 {
            l.s(" DEAD");
        } else if win.raw_or != win.sdk_or {
            l.s(" DIFFER");
        }
        scr.say(l.as_str());
    }
    w.end_section(sec);
}

fn mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "NONE",
        1 => "DIGITAL",
        2 => "ANALOG",
        3 => "NEGCON",
        4 => "CONFIG",
        _ => "UNKNOWN",
    }
}

// --------------------------------------------------------------------------- soaks

/// Cycles into the frame the soak's poll starts: 0 for the first hundred polls, as a loop that
/// polls first thing after the blank does, then sweeping through the whole frame.
fn poll_offset(i: usize) -> u32 {
    if i < 100 {
        0
    } else {
        ((i as u32) * 1_873) % 540_000
    }
}

/// Start CD XA playback, the SPU's CD input and the display flips the soak runs under.
struct Load {
    player: Option<Player>,
}

impl Load {
    #[inline(never)]
    fn start() -> Self {
        psx_spu::init();
        psx_spu::set_main_volume(psx_spu::Volume::MAX, psx_spu::Volume::MAX);
        psx_spu::set_cd_volume(psx_spu::CdVolume::MAX, psx_spu::CdVolume::MAX);
        psx_spu::enable_cd_audio(true);
        let mut player = None;
        if let Some(file) = xa_loop::find_file(xa_loop::SONG_FILE, xa_loop::FILE_NUMBER) {
            // SAFETY: nothing else drives the CD controller while the survey runs.
            let mut p = Player::new(unsafe { psx_io::periph::Cd::steal() });
            p.set_volume(0x30, 0x30);
            if p.play(file.song(0), true).is_ok() {
                player = Some(p);
            } else {
                let _ = p.release();
            }
        }
        Self { player }
    }

    fn per_frame(&mut self) {
        if let Some(p) = self.player.as_mut() {
            let _ = p.poll();
        }
    }

    fn stop(self) {
        if let Some(p) = self.player {
            let _ = p.release();
        }
        crate::run::cd_quiesce();
        crate::run::silence_spu();
    }
}

#[inline(never)]
fn run_soak(
    scr: &mut Screen,
    bus: &mut ControllerPort,
    port: Port,
    kind: u8,
    w: &mut Writer,
) {
    scr.begin(rec::SOAK_NAMES[kind as usize]);
    let mut soak = Soak::EMPTY;
    soak.kind = kind;
    // A fresh driver, as the launcher makes it, whose first poll runs the handshake.
    let mut pads = Controllers::new();
    let mut watch = Watch::new();
    let _ = sdk_poll(&mut pads, &mut watch, port);
    let configs_before = watch.configs;
    let mut load = if kind == 1 { Some(Load::start()) } else { None };
    let vb_start = interrupts::vblank_count();
    let mut shown = PollView::ZERO;
    for i in 0..SDK_POLLS {
        frame();
        if let Some(load) = load.as_mut() {
            load.per_frame();
        }
        // The screen is drawn first, right after the blank, from the poll before; the poll then
        // goes where the sweep puts it in the frame.
        if i > 0 && (kind == 1 || i % 6 == 0) {
            let mut a = L::new();
            a.s("POLL ").u(i as u32).s("/").u(SDK_POLLS as u32).s("  TIME LEFT ").u((SDK_POLLS - i) as u32 / 60 + 1).s(" S");
            let mut e = L::new();
            e.s("OK ").u(u32::from(soak.classes[0])).s(" SHORT ").u(u32::from(soak.classes[1])).s(" STALE ").u(u32::from(soak.stale));
            e.s(" CFG ").u(watch.configs - configs_before).s(" VB ").u(u32::from(soak.irq_failed));
            live_sdk(scr, "SDK POLLS, THE LAUNCHER LOOP", a.as_str(), &shown, None, e.as_str());
        }
        wait_ticks(bus, poll_offset(i));
        let vb0 = interrupts::vblank_count();
        let t0 = bus.clock();
        let sdk = sdk_poll(&mut pads, &mut watch, port);
        let ticks = bus.clock().wrapping_sub(t0);
        let irq = interrupts::vblank_count() != vb0;
        soak.note(sdk.view, &sdk.wire, sdk.failed, sdk.stale, irq, ticks);
        shown = sdk.view;
        if kind == 2 {
            let _ = transact(bus, port, &POLL_TX, POLL_PROFILE, 1, 0, 0);
        }
    }
    soak.vblanks = interrupts::vblank_count().wrapping_sub(vb_start) as u16;
    soak.config_txns = (watch.configs - configs_before) as u16;
    if let Some(load) = load {
        load.stop();
    }
    pads_release(&mut pads);
    let sec = w.begin_section(rec::TAG_SOAK);
    rec::put_soak(w, &soak);
    w.end_section(sec);
    say_soak(scr, &soak, kind);
}

/// The one-line verdict of a soak, and a second line for the timing.
fn say_soak(scr: &mut Screen, soak: &Soak, kind: u8) {
    let mut modal = 0usize;
    for (i, c) in soak.acked_hist.iter().enumerate() {
        if *c > soak.acked_hist[modal] {
            modal = i;
        }
    }
    let last = soak.runs[soak.run_n.saturating_sub(1) as usize].view;
    let mut l = L::new();
    l.s("S").u(u32::from(kind) + 1).s(" ").u(u32::from(soak.polls)).s(" POLLS: OK ").u(u32::from(soak.classes[0]));
    if soak.classes[1] != 0 {
        l.s(" SHORT ").u(u32::from(soak.classes[1])).s(" DIED AT ").u(modal as u32);
    }
    if soak.classes[2] + soak.classes[3] + soak.classes[4] != 0 {
        l.s(" BAD ").u(u32::from(soak.classes[2] + soak.classes[3] + soak.classes[4]));
    }
    scr.say(l.as_str());
    let mut l = L::new();
    l.s("SDK ").s(mode_name(last.mode)).s(if last.flags & 2 != 0 { " LOCKED" } else { "" });
    l.s(" BTN ").hex(u32::from(last.buttons), 4).s(" STALE ").u(u32::from(soak.stale));
    l.s(" CFG ").u(u32::from(soak.config_txns));
    scr.say(l.as_str());
}

/// Nothing to release in a `Controllers`; named so the soak reads as a lifetime.
fn pads_release<const A: bool>(_pads: &mut Controllers<A>) {}

// --------------------------------------------------------------------------- hand-over

#[inline(never)]
fn game_polls<const A: bool>(
    mut game: Controllers<A>,
    watch: &mut Watch,
    port: Port,
    out: &mut Handover,
    scr: &mut Screen,
) {
    let vb0 = interrupts::vblank_count();
    let first = sdk_poll(&mut game, watch, port);
    out.first_poll_vblanks = interrupts::vblank_count().wrapping_sub(vb0) as u16;
    out.note(0, first.view, first.failed);
    for i in 1..=GAME_POLLS {
        frame();
        let r = sdk_poll(&mut game, watch, port);
        out.note(i, r.view, r.failed);
        if i % 6 == 0 {
            let mut a = L::new();
            a.s("GAME POLL ").u(i as u32).s("/").u(GAME_POLLS as u32);
            let mut d = L::new();
            d.s("FIRST POLL ").u(u32::from(out.first_poll_vblanks)).s(" FRAMES  F3 POLLS ").u(u32::from(out.config_polls));
            live_sdk(scr, "AFTER THE HAND-OVER", a.as_str(), &r.view, None, d.as_str());
        }
    }
}

#[inline(never)]
fn run_handover(
    scr: &mut Screen,
    port: Port,
    variant: u8,
    baseline_irq: u32,
    baseline_dpcr: u32,
    w: &mut Writer,
) {
    scr.begin(rec::HANDOVER_NAMES[variant as usize]);
    let mut out = Handover::EMPTY;
    out.variant = variant;
    // The launcher: a driver whose first poll runs the handshake, then its loop.
    let mut launcher = Controllers::new();
    let mut watch = Watch::new();
    let mut last = sdk_poll(&mut launcher, &mut watch, port);
    for _ in 0..30 {
        frame();
        last = sdk_poll(&mut launcher, &mut watch, port);
    }
    out.launcher_mode = last.view.mode;
    out.launcher_analog = last.view.flags & 2 != 0;
    scr.live(&[("LAUNCHER LOOP DONE, QUIESCING", GOLD)]);
    // The chain loader's quiesce: SIO0 reset, timers off, interrupt mask zero, DMA aborted.
    psx_rt::entry::set_entry_state();
    interrupts::install_vblank_counter();
    // SAFETY: restoring the DMA control word the suite started with.
    unsafe { psx_io::write_u32(psx_hw::dma::DPCR, baseline_dpcr) };
    irq::set_mask(baseline_irq);
    let mut game_watch = Watch::new();
    if variant == 0 {
        game_polls(Controllers::new(), &mut game_watch, port, &mut out, scr);
    } else {
        game_polls(Controllers::digital(), &mut game_watch, port, &mut out, scr);
    }
    let sec = w.begin_section(rec::TAG_HANDOVER);
    rec::put_handover(w, &out);
    w.end_section(sec);
    let mut l = L::new();
    l.s("HO").u(u32::from(variant) + 1).s(" LAUNCHER ").s(mode_name(out.launcher_mode)).s(", GAME ANALOG AT ");
    if out.first_analog == 0xFFFF {
        l.s("NEVER");
    } else {
        l.u(u32::from(out.first_analog));
    }
    scr.say(l.as_str());
    let mut l = L::new();
    l.s("F3 POLLS ").u(u32::from(out.config_polls)).s(" FAIL ").u(u32::from(out.failed)).s(" 1ST POLL ").u(u32::from(out.first_poll_vblanks)).s(" FR");
    scr.say(l.as_str());
}

// --------------------------------------------------------------------------- records and pages

#[derive(Clone, Copy)]
struct Stored {
    len: u16,
    run_id: u16,
    record_no: u8,
    port: u8,
}

impl Stored {
    const EMPTY: Stored = Stored { len: 0, run_id: 0, record_no: 0, port: 0 };

    fn pages(&self) -> usize {
        usize::from(self.len).div_ceil(rec::CHARS_PER_PAGE / 4 * 3).max(1)
    }
}

static mut STORED: [Stored; RECORDS] = [Stored::EMPTY; RECORDS];

/// Slot 0 of the borrowed staging room is where a record is built; slots 1 to `RECORDS` hold the
/// finished ones.
fn slot_bytes(i: usize) -> &'static mut [u8] {
    const _: () = assert!(RECORD_MAX * (RECORDS + 1) <= crate::handoff_probe::STAGING_BYTES);
    // SAFETY: the slots are disjoint ranges of a buffer nothing else touches while the survey runs.
    unsafe {
        core::slice::from_raw_parts_mut(crate::handoff_probe::staging_ptr().add(i * RECORD_MAX), RECORD_MAX)
    }
}

/// Draw page `page` of a stored record as a QR symbol with its text header.
fn draw_page(scr: &Screen, s: &Stored, bytes: &[u8], page: usize) {
    let total = s.pages();
    let step = rec::CHARS_PER_PAGE / 4 * 3;
    let first = (page * step).min(usize::from(s.len));
    let end = (first + step).min(usize::from(s.len));
    let mut b64 = [0u8; rec::CHARS_PER_PAGE + 4];
    let n = base64_encode(&bytes[first..end], &mut b64);
    let chunk = core::str::from_utf8(&b64[..n]).unwrap_or("");
    let page_crc = crc32(chunk.as_bytes());
    let mut text = [0u8; 4 + 8 + 1 + rec::CHARS_PER_PAGE + 3 + 8 + 4];
    let mut len = 0usize;
    append(&mut text, &mut len, b"PXC/");
    append(&mut text, &mut len, hex2((page + 1) as u8).as_str().as_bytes());
    append(&mut text, &mut len, hex2(total as u8).as_str().as_bytes());
    append(&mut text, &mut len, hex4(s.run_id).as_str().as_bytes());
    append(&mut text, &mut len, b"/");
    append(&mut text, &mut len, chunk.as_bytes());
    append(&mut text, &mut len, b"/C:");
    append(&mut text, &mut len, hex8(page_crc).digits().as_bytes());
    let encoded = core::str::from_utf8(&text[..len]).unwrap_or("");
    tty::println(encoded);
    let mut temp = [0u8; QR_VERSION_MAX.buffer_len()];
    let mut out = [0u8; QR_VERSION_MAX.buffer_len()];
    let qr = QrCode::encode_text(
        encoded,
        &mut temp,
        &mut out,
        QrCodeEcc::Low,
        QR_VERSION_MIN,
        QR_VERSION_MAX,
        None,
        false,
    );
    let mut modules = [0u8; (QR_SIZE * QR_SIZE).div_ceil(8)];
    let mut size = 0usize;
    if let Ok(qr) = &qr {
        size = qr.size() as usize;
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                if qr.get_module(x, y) {
                    let bit = y as usize * QR_SIZE + x as usize;
                    modules[bit / 8] |= 1 << (bit & 7);
                }
            }
        }
    }
    let crc_all = if s.len >= 4 {
        u32::from_le_bytes([
            bytes[usize::from(s.len) - 4],
            bytes[usize::from(s.len) - 3],
            bytes[usize::from(s.len) - 2],
            bytes[usize::from(s.len) - 1],
        ])
    } else {
        0
    };
    both(&mut || {
        rect(0, 0, 320, 240, (255, 255, 255));
        rect(0, 0, 320, 24, (6, 8, 18));
        let mut l = L::new();
        l.s("PXC RUN ").hex(u32::from(s.run_id), 4).s(" REC ").u(u32::from(s.record_no));
        l.s(" PORT ").u(u32::from(s.port) + 1).s(" PAGE ").u(page as u32 + 1).s("/").u(total as u32);
        scr.text(0, 2, l.as_str(), GOOD);
        let mut l = L::new();
        l.s("PG ").hex(page_crc, 8).s(" ALL ").hex(crc_all, 8);
        scr.text(0, 13, l.as_str(), DIM);
        if size == 0 {
            scr.text(80, 112, "QR ENCODE FAILED", (255, 96, 96));
        } else {
            let modules_n = size as i16;
            let scale = (QR_AREA / (modules_n + QR_QUIET * 2)).max(1);
            draw_qr(&modules, QR_SIZE, size, 28, scale);
        }
    });
}

/// Finish the record in `w` (CRC), store it and return its slot.
fn store_record(slot: &mut Stored, bytes: &mut [u8], w: &Writer, hdr: &Header) {
    let mut crc_buf = [0u8; 4];
    let crc = crc32(w.bytes());
    crc_buf.copy_from_slice(&crc.to_le_bytes());
    let n = w.len();
    bytes[..n].copy_from_slice(w.bytes());
    if n + 4 <= RECORD_MAX {
        bytes[n..n + 4].copy_from_slice(&crc_buf);
        slot.len = (n + 4) as u16;
    } else {
        slot.len = n as u16;
    }
    slot.run_id = (u16::from(hdr.boot_id) << 8) | u16::from(hdr.record_no);
    slot.record_no = hdr.record_no;
    slot.port = hdr.port;
}

struct Survey {
    scr: Screen,
    boot_id: u8,
    records: u8,
    baseline_irq: u32,
    baseline_dpcr: u32,
}

/// The whole record for the pad in `port` (or for nothing answering).
fn run_record(sv: &mut Survey, port: Port) {
    // SAFETY: a token is a logic guard; nothing else drives SIO0 while the survey runs.
    let mut bus = unsafe { ControllerPort::steal() };
    sv.records = sv.records.wrapping_add(1);
    sv.scr.record_no = sv.records;
    sv.scr.log_n = 0;
    sv.scr.port = u8::from(port.is_two());
    // SAFETY: single thread; the buffer is only used inside this function.
    let buf = slot_bytes(0);
    let mut w = Writer::new(buf);
    let mut hdr = Header {
        suite_major: crate::SUITE_VERSION_MAJOR,
        suite_minor: crate::SUITE_VERSION_MINOR,
        record_no: sv.records,
        boot_id: sv.boot_id,
        port: u8::from(port.is_two()),
        kind: 0,
        flags: 0,
        sdk_rev: SDK_REV,
        timer_hz: TIMER_HZ,
        ack_timeout: ACK_TIMEOUT,
    };
    rec::put_header(&mut w, &hdr);
    let res = run_survey(&mut sv.scr, &mut bus, port, &mut w);
    if !res.any_ack {
        hdr.kind = 1;
    }
    if hdr.kind == 0 {
        let mut pads = Controllers::new();
        let mut watch = Watch::new();
        // The launcher's first poll, before anything draws, runs the handshake.
        let _ = sdk_poll(&mut pads, &mut watch, port);
        run_windows(&mut sv.scr, &mut bus, port, &mut pads, &mut watch, &mut w);
        run_soak(&mut sv.scr, &mut bus, port, 0, &mut w);
        run_soak(&mut sv.scr, &mut bus, port, 1, &mut w);
        run_soak(&mut sv.scr, &mut bus, port, 2, &mut w);
        run_handover(&mut sv.scr, port, 0, sv.baseline_irq, sv.baseline_dpcr, &mut w);
        run_handover(&mut sv.scr, port, 1, sv.baseline_irq, sv.baseline_dpcr, &mut w);
    }
    if w.overflow {
        hdr.flags |= 1;
    }
    rec::patch_flags(&mut w, hdr.flags);
    let slot_index = usize::from(sv.records.wrapping_sub(1)) % RECORDS;
    // SAFETY: single thread; the page cycle reads the slots only between records.
    let slot = unsafe { &mut (*core::ptr::addr_of_mut!(STORED))[slot_index] };
    store_record(slot, slot_bytes(1 + slot_index), &w, &hdr);
}

/// Poll `port` raw for presence: a complete poll reply.
fn present(bus: &mut ControllerPort, port: Port) -> bool {
    // Anything that acknowledges the address byte counts, so a pad whose polls do not complete is
    // still surveyed.
    let t = transact(bus, port, &POLL_TX, POLL_PROFILE, 1, 0, 0);
    t.sent >= 1 && t.rise[0] != 0xFFFF
}

/// The mode: runs until the console is reset.
pub(crate) fn execute(baseline_irq: u32, baseline_dpcr: u32) -> ! {
    interrupts::install_vblank_counter();
    irq::set_mask(baseline_irq);
    let _ = Gpu::new(
        crate::probe_gpu_dma(),
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let boot_id = (interrupts::vblank_count() as u8) ^ 0xA5;
    let mut sv = Survey {
        scr: Screen::new(),
        boot_id,
        records: 0,
        baseline_irq,
        baseline_dpcr,
    };
    tty::println("pad-survey: begins");
    // SAFETY: a token is a logic guard; nothing else drives SIO0 while the survey runs.
    let mut bus = unsafe { ControllerPort::steal() };
    let ports = [Port::One, Port::Two];
    let mut seen = [false; 2];
    let mut streak = [0u8; 2];
    let mut empty_logged = [false; 2];
    let mut waited = 0u32;
    let mut view_record = 0usize;
    let mut view_page = 0usize;
    let mut page_frames = PAGE_FRAMES;
    let mut shown: Option<(usize, usize)> = None;
    loop {
        frame();
        waited += 1;
        let mut starts = [false; 2];
        for p in 0..2 {
            let now = present(&mut bus, ports[p]);
            // A change is believed after PRESENCE_HOLD polls in a row that agree.
            if now != seen[p] {
                streak[p] += 1;
                if streak[p] >= PRESENCE_HOLD {
                    seen[p] = now;
                    streak[p] = 0;
                    if now {
                        starts[p] = true;
                    } else {
                        empty_logged[p] = false;
                    }
                }
            } else {
                streak[p] = 0;
            }
        }
        if !starts[0] && !starts[1] && waited >= NO_PAD_FRAMES && sv.records == 0 {
            // Nothing answered for the wait: record that on port 1, then port 2.
            for p in 0..2 {
                if !empty_logged[p] && !seen[p] {
                    empty_logged[p] = true;
                    run_record(&mut sv, ports[p]);
                }
            }
            shown = None;
            view_record = 0;
            view_page = 0;
        }
        for p in 0..2 {
            if !starts[p] {
                continue;
            }
            empty_logged[p] = true;
            run_record(&mut sv, ports[p]);
            seen[p] = true;
            streak[p] = 0;
            // Show the new record first.
            view_record = usize::from(sv.records.wrapping_sub(1)) % RECORDS;
            view_page = 0;
            page_frames = PAGE_FRAMES;
            shown = None;
        }
        // The page cycle, or the wait screen before there is a record.
        // SAFETY: single thread; read between records.
        let stored = unsafe { &*core::ptr::addr_of!(STORED) };
        let have = usize::from(sv.records).min(RECORDS);
        if have == 0 {
            if waited % 30 == 1 {
                let left = NO_PAD_FRAMES.saturating_sub(waited) / 60;
                sv.scr.record_no = 0;
                sv.scr.begin("WAITING FOR A CONTROLLER");
                let mut a = L::new();
                a.s("PLUG A CONTROLLER IN PORT 1 OR 2.");
                let mut b = L::new();
                b.s("NO CONTROLLER IN ").u(left).s(" S: RECORD THAT.");
                sv.scr.live(&[
                    (a.as_str(), GOLD),
                    (b.as_str(), INK),
                    ("NOTHING TO PRESS. UNPLUG AND PLUG TO", DIM),
                    ("START ANOTHER RECORD.", DIM),
                ]);
            }
            continue;
        }
        page_frames += 1;
        if page_frames >= PAGE_FRAMES {
            page_frames = 0;
            let s = &stored[view_record];
            if shown != Some((view_record, view_page)) {
                draw_page(&sv.scr, s, slot_bytes(1 + view_record), view_page);
                shown = Some((view_record, view_page));
            }
            view_page += 1;
            if view_page >= s.pages() {
                view_page = 0;
                let mut next = view_record;
                loop {
                    next = (next + 1) % RECORDS;
                    if stored[next].len != 0 || next == view_record {
                        break;
                    }
                }
                view_record = next;
            }
        }
    }
}

/// A line of text under construction. The builders are out of line on purpose: this module
/// builds a few hundred lines and inlining each append would cost the boot EXE tens of kilobytes.
struct L(Line);

impl L {
    fn new() -> Self {
        L(Line::new())
    }

    #[inline(never)]
    fn s(&mut self, t: &str) -> &mut Self {
        self.0.s(t);
        self
    }

    #[inline(never)]
    fn u(&mut self, v: u32) -> &mut Self {
        self.0.u(v);
        self
    }

    #[inline(never)]
    fn hex(&mut self, v: u32, digits: u32) -> &mut Self {
        self.0.hex(v, digits);
        self
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

const CLASS_NAMES: [&str; 5] = ["OK", "SHORT", "GARBLED", "NO PAD", "NO EXCHANGE"];

/// The live screen every phase shares: two heading lines, the bytes on the wire for the SDK's
/// poll (and the raw layer's, in a window), how the exchange ended and how many bytes were
/// acknowledged, and what the SDK reported.
#[inline(never)]
fn live_sdk(
    scr: &Screen,
    head: &str,
    sub: &str,
    v: &PollView,
    raw: Option<&Txn>,
    extra: &str,
) {
    let mut wire = L::new();
    wire.s("SDK WIRE ");
    hex_bytes(&mut wire, &v.rx[..v.n.min(9) as usize]);
    let mut rawl = L::new();
    if let Some(raw) = raw {
        rawl.s("RAW WIRE ");
        hex_bytes(&mut rawl, &raw.rx[..raw.got.min(9) as usize]);
    }
    let mut how = L::new();
    how.s(CLASS_NAMES[usize::from(v.class).min(4)]).s(" ACKED ").u(u32::from(v.acked)).s(" OF ").u(u32::from(v.n));
    if v.irq_after_clear {
        how.s(" IRQ LEFT");
    }
    if v.early != 0 {
        how.s(" EARLY ").u(u32::from(v.early) - 1);
    }
    let mut rep = L::new();
    rep.s("REPORTS ").s(mode_name(v.mode)).s(if v.flags & 2 != 0 { " LOCKED" } else { "" });
    rep.s(" BTN ").hex(u32::from(v.buttons), 4);
    let mut st = L::new();
    st.s("STICKS RX ").hex(u32::from(v.sticks[0]), 2).s(" RY ").hex(u32::from(v.sticks[1]), 2);
    st.s(" LX ").hex(u32::from(v.sticks[2]), 2).s(" LY ").hex(u32::from(v.sticks[3]), 2);
    scr.live(&[
        (head, GOLD),
        (sub, GOOD),
        (wire.as_str(), INK),
        (rawl.as_str(), INK),
        (how.as_str(), INK),
        (rep.as_str(), INK),
        (st.as_str(), INK),
        (extra, DIM),
    ]);
}

/// Window names short enough for a verdict line.
const SHORT_NAMES: [&str; 8] = ["REST", "X", "DPAD", "LSTK", "RSTK", "L3R3", "ANLG", "OTHER"];

/// Which 240-line buffer the display shows: 0 or 240.
static mut FRONT: i16 = 0;

/// Draw a screen into the buffer that is not on show, then have the display switch to it at the
/// next VBlank, as the present path does (a queued GP1 display-start word the VBlank handler
/// applies). Nothing is ever seen half drawn. Both buffers hold the static parts of the screen
/// (`Screen::begin`, the log), so only the live area is redrawn here.
#[inline(never)]
fn present_scene(draw: &mut dyn FnMut()) {
    // SAFETY: single thread; plain static.
    let back = 240 - unsafe { FRONT };
    {
        probe_gpu!(gpu);
        gpu_io::wait_command_ready();
        gpu.set_draw_area((0, back as u16), (319, back as u16 + 239));
        gpu.set_draw_offset((0, back));
        draw();
        gpu_io::wait_command_ready();
        gpu.set_draw_offset((0, 0));
    }
    interrupts::queue_display_control_at_vblank(0x0500_0000 | ((back as u32) << 10));
    // SAFETY: as above.
    unsafe { FRONT = back };
}
