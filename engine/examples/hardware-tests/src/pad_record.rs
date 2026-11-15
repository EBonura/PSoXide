// SPDX-License-Identifier: GPL-2.0-or-later
//! The controller survey's record format, `PXCB` schema 1: one controller, one binary.
//!
//! This file is `core` only and is compiled twice: into the hardware-test disc, which encodes
//! records with it, and into `psoxide-hwtest` (by path), whose tests encode with the same code
//! the console runs and decode with the host reader. Nothing here touches hardware.
//!
//! Layout, little-endian:
//!
//! ```text
//! header, 32 bytes
//!    0  "PXCB"
//!    4  schema (1)
//!    5  suite major        6  suite minor
//!    7  record number (1-based, counts controllers seen since boot)
//!    8  boot id            9  port (0 = port 1, 1 = port 2)
//!   10  kind (0 pad answered, 1 nothing answered)
//!   11  flags (bit 0: the record outgrew its buffer and later data was dropped)
//!   12  SDK revision, 8 ASCII bytes
//!   20  timer (0: root counter 0, the system clock)   21  reserved
//!   22  timer rate in Hz, u32
//!   26  ACK timeout in timer ticks, u16
//!   28  reserved, 4 bytes
//! sections, each: tag u8, length u16, payload; until the last four bytes
//! CRC-32 of everything before it, u32
//! ```
//!
//! Section payloads are documented at their `put_*` function.

#![allow(dead_code)]

/// Record magic.
pub const MAGIC: &[u8; 4] = b"PXCB";
/// Schema version of this layout.
pub const SCHEMA: u8 = 1;
/// Header length in bytes.
pub const HEADER_LEN: usize = 32;
/// Transaction log section.
pub const TAG_TXN: u8 = 1;
/// Guided input window section.
pub const TAG_WINDOW: u8 = 2;
/// SDK soak section.
pub const TAG_SOAK: u8 = 3;
/// Hand-over section.
pub const TAG_HANDOVER: u8 = 4;
/// Most bytes one logged transaction holds.
pub const TXN_MAX: usize = 24;
/// Longest ID run list a window, soak or hand-over carries.
pub const ID_RUNS: usize = 8;
/// Mode run list length.
pub const MODE_RUNS: usize = 8;
/// Changed-state runs a soak keeps.
pub const SOAK_RUNS: usize = 12;
/// Failed polls a soak keeps in full.
pub const SOAK_FAILS: usize = 6;
/// Polls a soak makes at most (the failure bitmap holds this many).
pub const SOAK_POLLS_MAX: usize = 320;
/// Polls a hand-over makes at most.
pub const HANDOVER_POLLS_MAX: usize = 128;
/// Polls kept in full at the start of a hand-over.
pub const HANDOVER_SAMPLES: usize = 8;

/// Bounded little-endian writer. Writing past the end sets `overflow` and drops the bytes, so a
/// record that outgrew its buffer is flagged instead of panicking on a console.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    len: usize,
    /// Set by the first write that did not fit.
    pub overflow: bool,
}

impl<'a> Writer<'a> {
    /// A writer over `buf`, empty.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0, overflow: false }
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is written.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes still free.
    pub fn room(&self) -> usize {
        self.buf.len() - self.len
    }

    /// The bytes written.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Append bytes, or flag overflow and drop them all.
    pub fn put(&mut self, data: &[u8]) {
        if self.overflow || self.len + data.len() > self.buf.len() {
            self.overflow = true;
            return;
        }
        self.buf[self.len..self.len + data.len()].copy_from_slice(data);
        self.len += data.len();
    }

    /// Append one byte.
    pub fn u8(&mut self, v: u8) {
        self.put(&[v]);
    }

    /// Append a halfword.
    pub fn u16(&mut self, v: u16) {
        self.put(&v.to_le_bytes());
    }

    /// Append a word.
    pub fn u32(&mut self, v: u32) {
        self.put(&v.to_le_bytes());
    }

    /// Overwrite the byte at `at`, which must already be written.
    pub fn patch_u8(&mut self, at: usize, v: u8) {
        if at < self.len {
            self.buf[at] = v;
        }
    }

    /// Open a section; returns the position to pass to [`Writer::end_section`].
    pub fn begin_section(&mut self, tag: u8) -> usize {
        self.u8(tag);
        let at = self.len;
        self.u16(0);
        at
    }

    /// Close a section opened at `at`, patching its length.
    pub fn end_section(&mut self, at: usize) {
        if self.overflow || at + 2 > self.len {
            return;
        }
        let body = (self.len - at - 2) as u16;
        self.buf[at..at + 2].copy_from_slice(&body.to_le_bytes());
    }
}

/// Fixed fields of the header.
#[derive(Clone, Copy)]
pub struct Header {
    /// Suite version, major.
    pub suite_major: u8,
    /// Suite version, minor.
    pub suite_minor: u8,
    /// Controller number since boot, from 1.
    pub record_no: u8,
    /// Boot id.
    pub boot_id: u8,
    /// 0 port 1, 1 port 2.
    pub port: u8,
    /// 0 a pad answered, 1 nothing answered.
    pub kind: u8,
    /// Bit 0: data dropped.
    pub flags: u8,
    /// SDK revision, ASCII.
    pub sdk_rev: [u8; 8],
    /// Timer rate in Hz.
    pub timer_hz: u32,
    /// ACK timeout in ticks.
    pub ack_timeout: u16,
}

/// Write the 32-byte header.
pub fn put_header(w: &mut Writer, h: &Header) {
    w.put(MAGIC);
    w.u8(SCHEMA);
    w.u8(h.suite_major);
    w.u8(h.suite_minor);
    w.u8(h.record_no);
    w.u8(h.boot_id);
    w.u8(h.port);
    w.u8(h.kind);
    w.u8(h.flags);
    w.put(&h.sdk_rev);
    w.u8(0);
    w.u8(0);
    w.u32(h.timer_hz);
    w.u16(h.ack_timeout);
    w.u32(0);
}

/// Patch the flags byte of an already written header.
pub fn patch_flags(w: &mut Writer, flags: u8) {
    w.patch_u8(11, flags);
}

/// One raw transaction as logged.
#[derive(Clone, Copy)]
pub struct Txn {
    /// Survey step id (see [`step_name`]).
    pub step: u8,
    /// Spacing profile, 0 to 3.
    pub prof: u8,
    /// Index within a repeated step (polls after config exit), from 0.
    pub seq: u8,
    /// Bit 0 `/ACK` stuck high before the first byte, bit 1 `/ACK` missing after a byte (the
    /// transaction ends there), bit 2 no reply byte, bit 3 stopped at the cap.
    pub flags: u8,
    /// Bytes written.
    pub sent: u8,
    /// Reply bytes read (at most `sent`).
    pub got: u8,
    /// Bytes written.
    pub tx: [u8; TXN_MAX],
    /// Bytes read back.
    pub rx: [u8; TXN_MAX],
    /// Timer ticks from the write of byte i to its `/ACK` rising, 0xFFFF when none came.
    pub rise: [u16; TXN_MAX],
    /// Ticks the `/ACK` stayed asserted (capped at 254), 0xFF when only the interrupt latch
    /// showed it.
    pub width: [u8; TXN_MAX],
}

impl Txn {
    /// An empty transaction.
    pub const EMPTY: Txn = Txn {
        step: 0,
        prof: 0,
        seq: 0,
        flags: 0,
        sent: 0,
        got: 0,
        tx: [0; TXN_MAX],
        rx: [0; TXN_MAX],
        rise: [0xFFFF; TXN_MAX],
        width: [0; TXN_MAX],
    };
}

/// Transaction section entry: step u8, (seq << 4 | prof) u8, flags u8, sent u8, got u8, then
/// per byte written: tx u8, rx u8 (0 when not read), rise u16, width u8.
pub fn put_txn(w: &mut Writer, t: &Txn) {
    w.u8(t.step);
    w.u8((t.seq << 4) | (t.prof & 0x0F));
    w.u8(t.flags);
    w.u8(t.sent);
    w.u8(t.got);
    for i in 0..t.sent as usize {
        w.u8(t.tx[i]);
        w.u8(if i < t.got as usize { t.rx[i] } else { 0 });
        w.u16(t.rise[i]);
        w.u8(t.width[i]);
    }
}

/// Names of the survey steps, indexed by step id.
pub const STEP_NAMES: [&str; 22] = [
    "?",
    "POLL 42 AS POWERED",
    "ENTER CONFIG 43",
    "STATUS 45",
    "CONST 46 PAGE 0",
    "CONST 46 PAGE 1",
    "CONST 47",
    "CONST 4C PAGE 0",
    "CONST 4C PAGE 1",
    "SET ANALOG LOCK 44",
    "POLL 42 IN CONFIG",
    "MAP MOTORS 4D",
    "EXIT CONFIG 43",
    "POLL 42 AFTER EXIT",
    "ENTER CONFIG 43 (2)",
    "SET DIGITAL 44",
    "EXIT CONFIG 43 (2)",
    "POLL 42 AFTER EXIT (2)",
    "ENTER CONFIG 43 (3)",
    "SET ANALOG LOCK 44 (3)",
    "EXIT CONFIG 43 (3)",
    "POLL 42 AFTER EXIT (3)",
];

/// Name of a step id.
pub fn step_name(step: u8) -> &'static str {
    STEP_NAMES.get(step as usize).copied().unwrap_or("?")
}

/// Spacing profile names, indexed by profile.
pub const PROFILE_NAMES: [&str; 4] = ["MIN", "SHORT", "MEDIUM", "LONG"];

/// An ID and how many polls in a row answered it.
#[derive(Clone, Copy, Default)]
pub struct IdRun {
    /// ID byte.
    pub id: u8,
    /// Polls.
    pub count: u16,
}

/// A mode the SDK reported and for how many polls.
#[derive(Clone, Copy, Default)]
pub struct ModeRun {
    /// Mode code: 0 disconnected, 1 digital, 2 analog, 3 neGcon, 4 config, 5 unknown.
    pub mode: u8,
    /// `Controllers::analog()` said Analog.
    pub analog: bool,
    /// Polls.
    pub count: u16,
}

/// One stick axis seen on the wire.
#[derive(Clone, Copy)]
pub struct Axis {
    /// Smallest byte seen.
    pub min: u8,
    /// Largest byte seen.
    pub max: u8,
    /// Bytes at four fixed moments of the window.
    pub samples: [u8; 4],
}

/// Window numbers.
pub const WINDOW_NAMES: [&str; 8] = [
    "HANDS OFF",
    "HOLD X",
    "EACH D-PAD DIRECTION",
    "LEFT STICK CIRCLE",
    "RIGHT STICK CIRCLE",
    "L3 AND R3",
    "ANALOG BUTTON",
    "OTHER BUTTONS",
];

/// Everything a guided window kept, raw wire against the SDK's report, same frames.
#[derive(Clone, Copy)]
pub struct Window {
    /// Index into [`WINDOW_NAMES`].
    pub phase: u8,
    /// Frames planned.
    pub frames_planned: u16,
    /// Frames run.
    pub frames_run: u16,
    /// Raw polls whose reply was complete and carried the 0x5A magic.
    pub raw_ok: u16,
    /// Raw polls that were not.
    pub raw_bad: u16,
    /// ID byte runs seen on the wire.
    pub raw_ids: [IdRun; ID_RUNS],
    /// Valid entries of `raw_ids`.
    pub raw_id_n: u8,
    /// OR of the active-high button bits seen on the wire.
    pub raw_or: u16,
    /// AND of them (bits held the whole time).
    pub raw_and: u16,
    /// Right X, right Y, left X, left Y as the wire carries them.
    pub raw_axis: [Axis; 4],
    /// SDK polls whose last exchange was complete.
    pub sdk_ok: u16,
    /// SDK polls whose last exchange was not (a poll the SDK answered with its stored state).
    pub sdk_bad: u16,
    /// Of those, polls after which the SDK still reported analog.
    pub sdk_stale: u16,
    /// Frames the SDK reported a connected pad.
    pub sdk_conn: u16,
    /// Frames the SDK reported analog mode.
    pub sdk_analog: u16,
    /// Mode runs the SDK reported.
    pub sdk_modes: [ModeRun; MODE_RUNS],
    /// Valid entries of `sdk_modes`.
    pub sdk_mode_n: u8,
    /// OR of the buttons the SDK reported.
    pub sdk_or: u16,
    /// AND of them.
    pub sdk_and: u16,
    /// Smallest stick byte the SDK reported: right X, right Y, left X, left Y.
    pub sdk_min: [u8; 4],
    /// Largest.
    pub sdk_max: [u8; 4],
}

impl Window {
    /// A window with nothing seen.
    pub const EMPTY: Window = Window {
        phase: 0,
        frames_planned: 0,
        frames_run: 0,
        raw_ok: 0,
        raw_bad: 0,
        raw_ids: [IdRun { id: 0, count: 0 }; ID_RUNS],
        raw_id_n: 0,
        raw_or: 0,
        raw_and: 0xFFFF,
        raw_axis: [Axis { min: 0xFF, max: 0, samples: [0; 4] }; 4],
        sdk_ok: 0,
        sdk_bad: 0,
        sdk_stale: 0,
        sdk_conn: 0,
        sdk_analog: 0,
        sdk_modes: [ModeRun { mode: 0, analog: false, count: 0 }; MODE_RUNS],
        sdk_mode_n: 0,
        sdk_or: 0,
        sdk_and: 0xFFFF,
        sdk_min: [0xFF; 4],
        sdk_max: [0; 4],
    };
}

/// Append `id` to a run list, extending the last run when it repeats. A full list drops new
/// runs (the counts of what was kept stay true, the totals of the poll counters say how many).
pub fn push_id(runs: &mut [IdRun], n: &mut u8, id: u8) {
    let len = *n as usize;
    if len > 0 && runs[len - 1].id == id {
        runs[len - 1].count = runs[len - 1].count.saturating_add(1);
    } else if len < runs.len() {
        runs[len] = IdRun { id, count: 1 };
        *n += 1;
    }
}

/// [`push_id`] for modes.
pub fn push_mode(runs: &mut [ModeRun], n: &mut u8, mode: u8, analog: bool) {
    let len = *n as usize;
    if len > 0 && runs[len - 1].mode == mode && runs[len - 1].analog == analog {
        runs[len - 1].count = runs[len - 1].count.saturating_add(1);
    } else if len < runs.len() {
        runs[len] = ModeRun { mode, analog, count: 1 };
        *n += 1;
    }
}

fn put_ids(w: &mut Writer, runs: &[IdRun; ID_RUNS], n: u8) {
    w.u8(n);
    for r in &runs[..n as usize] {
        w.u8(r.id);
        w.u16(r.count);
    }
}

fn put_modes(w: &mut Writer, runs: &[ModeRun; MODE_RUNS], n: u8) {
    w.u8(n);
    for r in &runs[..n as usize] {
        w.u8(r.mode | if r.analog { 0x80 } else { 0 });
        w.u16(r.count);
    }
}

/// Window entry: phase u8, planned u16, run u16, raw_ok u16, raw_bad u16, id runs (n u8, then
/// id u8 + count u16 each), raw or u16, raw and u16, four axes (min, max, four samples), sdk
/// ok, bad, stale, connected, analog (u16 each), mode runs (n u8, then mode|0x80 if analog() u8
/// + count u16), sdk or u16, sdk and u16, sdk min x4, sdk max x4.
pub fn put_window(w: &mut Writer, s: &Window) {
    w.u8(s.phase);
    w.u16(s.frames_planned);
    w.u16(s.frames_run);
    w.u16(s.raw_ok);
    w.u16(s.raw_bad);
    put_ids(w, &s.raw_ids, s.raw_id_n);
    w.u16(s.raw_or);
    w.u16(s.raw_and);
    for a in &s.raw_axis {
        w.u8(a.min);
        w.u8(a.max);
        w.put(&a.samples);
    }
    w.u16(s.sdk_ok);
    w.u16(s.sdk_bad);
    w.u16(s.sdk_stale);
    w.u16(s.sdk_conn);
    w.u16(s.sdk_analog);
    put_modes(w, &s.sdk_modes, s.sdk_mode_n);
    w.u16(s.sdk_or);
    w.u16(s.sdk_and);
    w.put(&s.sdk_min);
    w.put(&s.sdk_max);
}

/// What `Controllers` reported after a poll, with the bytes the wire carried for it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct PollView {
    /// Reply bytes read, 0 when the poll made no exchange.
    pub n: u8,
    /// Reply bytes (ID at 1, magic at 2, buttons at 3 and 4, sticks at 5 to 8).
    pub rx: [u8; 9],
    /// Mode code as in [`ModeRun`].
    pub mode: u8,
    /// bit 0 connected, bit 1 `analog()` said Analog.
    pub flags: u8,
    /// Active-high buttons the SDK reported.
    pub buttons: u16,
    /// Right X, right Y, left X, left Y the SDK reported.
    pub sticks: [u8; 4],
    /// How the exchange ended, from the wire: 0 complete, 1 short (a byte or its `/ACK` missing), 2
    /// garbled (no 0x5A), 3 nobody answered, 4 the poll made no exchange.
    pub class: u8,
    /// Bytes whose `/ACK` was seen. A complete poll acknowledges every byte but the last, so a
    /// short one died at this byte (counting from 0, the address byte).
    pub acked: u8,
    /// Index plus one of the first byte whose `/ACK` rose before its reply was in (under 1,000
    /// ticks), 0 for none.
    pub early: u8,
    /// `STAT` still showed the `/ACK` interrupt latch after it was cleared.
    pub irq_after_clear: bool,
}

impl PollView {
    /// Nothing seen.
    pub const ZERO: PollView = PollView {
        n: 0,
        rx: [0; 9],
        mode: 0,
        flags: 0,
        buttons: 0,
        sticks: [0; 4],
        class: 4,
        acked: 0,
        early: 0,
        irq_after_clear: false,
    };
}

/// Per-byte `/ACK` timing of one poll's transaction, in timer ticks.
#[derive(Clone, Copy)]
pub struct Wire {
    /// Write to `/ACK` rise, 0xFFFF when none.
    pub rise: [u16; 9],
    /// Ticks `/ACK` stayed up (capped 254), 0xFF when only the latch showed it, 0 unknown.
    pub width: [u8; 9],
}

impl Wire {
    /// No bytes.
    pub const NONE: Wire = Wire { rise: [0xFFFF; 9], width: [0; 9] };
}

/// `n` + 9 rx + mode + flags + buttons + 4 sticks + class + acked + early + irq.
fn put_view(w: &mut Writer, v: &PollView) {
    w.u8(v.n);
    w.put(&v.rx);
    w.u8(v.mode);
    w.u8(v.flags);
    w.u16(v.buttons);
    w.put(&v.sticks);
    w.u8(v.class);
    w.u8(v.acked);
    w.u8(v.early);
    w.u8(u8::from(v.irq_after_clear));
}

/// A run of consecutive polls that read the same.
#[derive(Clone, Copy, Default)]
pub struct PollRun {
    /// Polls in the run.
    pub count: u16,
    /// What they read.
    pub view: PollView,
}

/// A failed poll kept in full.
#[derive(Clone, Copy)]
pub struct Failure {
    /// Poll index.
    pub poll: u16,
    /// What the SDK reported, with the wire bytes.
    pub view: PollView,
    /// Per-byte `/ACK` timing of that poll.
    pub wire: Wire,
}

/// Soak kinds.
pub const SOAK_NAMES: [&str; 3] = [
    "SOAK 1 LAUNCHER LOOP ALONE",
    "SOAK 2 LOOP + CD XA + SPU + FLIPS",
    "SOAK 3 LOOP + RAW POLL, SAME FRAME",
];

/// A few hundred consecutive `Controllers` polls.
#[derive(Clone, Copy)]
pub struct Soak {
    /// Index into [`SOAK_NAMES`].
    pub kind: u8,
    /// Polls made.
    pub polls: u16,
    /// Polls whose last exchange was complete.
    pub ok: u16,
    /// Polls that were not, or made no exchange.
    pub failed: u16,
    /// Failed polls after which the SDK still reported analog.
    pub stale: u16,
    /// Failed polls during which a VBlank interrupt ran.
    pub irq_failed: u16,
    /// Configuration transactions (a handshake or its retry) the polls caused.
    pub config_txns: u16,
    /// Video frames the soak took.
    pub vblanks: u16,
    /// Shortest and longest `Controllers::poll` call in timer ticks (capped at 0xFFFF).
    pub poll_min: u16,
    /// Longest.
    pub poll_max: u16,
    /// Bit i set when poll i failed.
    pub failed_bits: [u8; SOAK_POLLS_MAX / 8],
    /// ID runs seen on the wire (0xFF for a poll with no exchange).
    pub ids: [IdRun; ID_RUNS],
    /// Valid entries.
    pub id_n: u8,
    /// Runs of identical reads, in order.
    pub runs: [PollRun; SOAK_RUNS],
    /// Valid entries.
    pub run_n: u8,
    /// Runs that did not fit.
    pub runs_dropped: u16,
    /// First failed polls in full.
    pub fails: [Failure; SOAK_FAILS],
    /// Valid entries.
    pub fail_n: u8,
    /// Polls by outcome class (see [`PollView::class`]).
    pub classes: [u16; 5],
    /// Polls by bytes acknowledged, 0 to 9.
    pub acked_hist: [u16; 10],
    /// Smallest and largest write-to-`/ACK` ticks seen at each byte index.
    pub rise_min: [u16; 9],
    /// Largest.
    pub rise_max: [u16; 9],
    /// Smallest `/ACK` width at each byte index (latch-only polls left out).
    pub width_min: [u8; 9],
    /// Largest.
    pub width_max: [u8; 9],
    /// Polls where only the latched bit showed the `/ACK`.
    pub latch_polls: u16,
}

impl Soak {
    /// A soak with nothing seen.
    pub const EMPTY: Soak = Soak {
        kind: 0,
        polls: 0,
        ok: 0,
        failed: 0,
        stale: 0,
        irq_failed: 0,
        config_txns: 0,
        vblanks: 0,
        poll_min: 0xFFFF,
        poll_max: 0,
        failed_bits: [0; SOAK_POLLS_MAX / 8],
        ids: [IdRun { id: 0, count: 0 }; ID_RUNS],
        id_n: 0,
        runs: [PollRun {
            count: 0,
            view: PollView::ZERO,
        }; SOAK_RUNS],
        run_n: 0,
        runs_dropped: 0,
        fails: [Failure { poll: 0, view: PollView::ZERO, wire: Wire::NONE }; SOAK_FAILS],
        classes: [0; 5],
        acked_hist: [0; 10],
        rise_min: [0xFFFF; 9],
        rise_max: [0; 9],
        width_min: [0xFF; 9],
        width_max: [0; 9],
        latch_polls: 0,
        fail_n: 0,
    };

    /// Record one poll.
    pub fn note(
        &mut self,
        view: PollView,
        wire: &Wire,
        failed: bool,
        stale: bool,
        irq: bool,
        ticks: u16,
    ) {
        let index = self.polls as usize;
        self.classes[(view.class as usize).min(4)] += 1;
        self.acked_hist[(view.acked as usize).min(9)] += 1;
        for b in 0..9 {
            if wire.rise[b] != 0xFFFF {
                self.rise_min[b] = self.rise_min[b].min(wire.rise[b]);
                self.rise_max[b] = self.rise_max[b].max(wire.rise[b]);
                if wire.width[b] == 0xFF {
                    self.latch_polls = self.latch_polls.saturating_add(1);
                } else if wire.width[b] != 0 {
                    self.width_min[b] = self.width_min[b].min(wire.width[b]);
                    self.width_max[b] = self.width_max[b].max(wire.width[b]);
                }
            }
        }
        self.polls = self.polls.saturating_add(1);
        self.poll_min = self.poll_min.min(ticks);
        self.poll_max = self.poll_max.max(ticks);
        push_id(&mut self.ids, &mut self.id_n, if view.n >= 2 { view.rx[1] } else { 0xFF });
        if failed {
            self.failed += 1;
            if stale {
                self.stale += 1;
            }
            if irq {
                self.irq_failed += 1;
            }
            if index < SOAK_POLLS_MAX {
                self.failed_bits[index / 8] |= 1 << (index % 8);
            }
            if (self.fail_n as usize) < SOAK_FAILS {
                self.fails[self.fail_n as usize] = Failure { poll: index as u16, view, wire: *wire };
                self.fail_n += 1;
            }
        } else {
            self.ok += 1;
        }
        let last = self.run_n as usize;
        if last > 0 && self.runs[last - 1].view == view {
            self.runs[last - 1].count = self.runs[last - 1].count.saturating_add(1);
        } else if last < SOAK_RUNS {
            self.runs[last] = PollRun { count: 1, view };
            self.run_n += 1;
        } else {
            self.runs_dropped = self.runs_dropped.saturating_add(1);
        }
    }
}

/// Soak entry: kind u8, polls, ok, failed, stale, irq_failed, config_txns, vblanks, poll_min,
/// poll_max (u16 each), classes (5 u16), acked histogram (10 u16), rise min and max (9 u16 each), width min and max
/// (9 u8 each), latch polls u16, failure bitmap (SOAK_POLLS_MAX / 8 bytes), id runs, run count u8 then per
/// run count u16 + view (n, 9 rx, mode, flags, buttons u16, 4 sticks), runs dropped u16, failure
/// count u8 then per failure poll u16 + view + 9 rise (u16) + 9 width (u8).
pub fn put_soak(w: &mut Writer, s: &Soak) {
    w.u8(s.kind);
    for v in [
        s.polls,
        s.ok,
        s.failed,
        s.stale,
        s.irq_failed,
        s.config_txns,
        s.vblanks,
        s.poll_min,
        s.poll_max,
    ] {
        w.u16(v);
    }
    for v in s.classes {
        w.u16(v);
    }
    for v in s.acked_hist {
        w.u16(v);
    }
    for v in s.rise_min {
        w.u16(v);
    }
    for v in s.rise_max {
        w.u16(v);
    }
    w.put(&s.width_min);
    w.put(&s.width_max);
    w.u16(s.latch_polls);
    w.put(&s.failed_bits);
    put_ids(w, &s.ids, s.id_n);
    w.u8(s.run_n);
    for r in &s.runs[..s.run_n as usize] {
        w.u16(r.count);
        put_view(w, &r.view);
    }
    w.u16(s.runs_dropped);
    w.u8(s.fail_n);
    for f in &s.fails[..s.fail_n as usize] {
        w.u16(f.poll);
        put_view(w, &f.view);
        for v in f.wire.rise {
            w.u16(v);
        }
        w.put(&f.wire.width);
    }
}

/// Hand-over variants.
pub const HANDOVER_NAMES: [&str; 2] = [
    "HAND-OVER, GAME Controllers::new()",
    "HAND-OVER, GAME Controllers::digital()",
];

/// The launcher-to-game hand-over: the launcher's loop reads the pad, `set_entry_state` resets
/// the machine, a fresh `Controllers` reads it as a game's first poll does.
#[derive(Clone, Copy)]
pub struct Handover {
    /// Index into [`HANDOVER_NAMES`].
    pub variant: u8,
    /// The launcher instance's mode code after its first poll.
    pub launcher_mode: u8,
    /// Its `analog()` said Analog.
    pub launcher_analog: bool,
    /// Video frames the game's first poll took.
    pub first_poll_vblanks: u16,
    /// Polls made after the first.
    pub polls: u16,
    /// Of them, failed or without exchange.
    pub failed: u16,
    /// Polls (first included) whose wire ID was 0xF3, the pad still in configuration mode.
    pub config_polls: u16,
    /// Index of the first poll after which the SDK reported analog, 0xFFFF if none.
    pub first_analog: u16,
    /// Bit i set when poll i failed.
    pub failed_bits: [u8; HANDOVER_POLLS_MAX / 8],
    /// ID runs.
    pub ids: [IdRun; ID_RUNS],
    /// Valid entries.
    pub id_n: u8,
    /// Mode runs.
    pub modes: [ModeRun; MODE_RUNS],
    /// Valid entries.
    pub mode_n: u8,
    /// The first polls in full.
    pub samples: [PollView; HANDOVER_SAMPLES],
    /// Valid entries.
    pub sample_n: u8,
    /// The last poll in full.
    pub last: PollView,
}

impl Handover {
    /// A hand-over with nothing seen.
    pub const EMPTY: Handover = Handover {
        variant: 0,
        launcher_mode: 0,
        launcher_analog: false,
        first_poll_vblanks: 0,
        polls: 0,
        failed: 0,
        config_polls: 0,
        first_analog: 0xFFFF,
        failed_bits: [0; HANDOVER_POLLS_MAX / 8],
        ids: [IdRun { id: 0, count: 0 }; ID_RUNS],
        id_n: 0,
        modes: [ModeRun { mode: 0, analog: false, count: 0 }; MODE_RUNS],
        mode_n: 0,
        samples: [PollView::ZERO;
            HANDOVER_SAMPLES],
        sample_n: 0,
        last: PollView::ZERO,
    };

    /// Record poll number `index` (0 is the first).
    pub fn note(&mut self, index: usize, view: PollView, failed: bool) {
        if index > 0 {
            self.polls = self.polls.saturating_add(1);
        }
        if failed {
            if index > 0 {
                self.failed += 1;
            }
            if index < HANDOVER_POLLS_MAX {
                self.failed_bits[index / 8] |= 1 << (index % 8);
            }
        }
        let id = if view.n >= 2 { view.rx[1] } else { 0xFF };
        if id == 0xF3 {
            self.config_polls += 1;
        }
        push_id(&mut self.ids, &mut self.id_n, id);
        push_mode(&mut self.modes, &mut self.mode_n, view.mode, view.flags & 2 != 0);
        if self.first_analog == 0xFFFF && view.flags & 2 != 0 {
            self.first_analog = index as u16;
        }
        if (self.sample_n as usize) < HANDOVER_SAMPLES {
            self.samples[self.sample_n as usize] = view;
            self.sample_n += 1;
        }
        self.last = view;
    }
}

/// Hand-over entry: variant, launcher mode | 0x80 if analog(), first poll vblanks u16, polls,
/// failed, config polls, first analog (u16 each), failure bitmap, id runs, mode runs, sample
/// count u8 then views, last view.
pub fn put_handover(w: &mut Writer, h: &Handover) {
    w.u8(h.variant);
    w.u8(h.launcher_mode | if h.launcher_analog { 0x80 } else { 0 });
    w.u16(h.first_poll_vblanks);
    w.u16(h.polls);
    w.u16(h.failed);
    w.u16(h.config_polls);
    w.u16(h.first_analog);
    w.put(&h.failed_bits);
    put_ids(w, &h.ids, h.id_n);
    put_modes(w, &h.modes, h.mode_n);
    w.u8(h.sample_n);
    for v in &h.samples[..h.sample_n as usize] {
        put_view(w, v);
    }
    put_view(w, &h.last);
}

/// Length of a PX-style page chunk in base64 characters: the same 36 by 23 the capture pages use.
pub const CHARS_PER_PAGE: usize = 36 * 23;
/// Page schema prefix of a controller record.
pub const PAGE_SCHEMA: &str = "PXC";
