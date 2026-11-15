// SPDX-License-Identifier: GPL-2.0-or-later
//! The main menu, with the pad readout, the pad lab and the power-on record.
//!
//! The menu runs BEFORE the engine, like the launcher: it makes its own `psx_pad::Controllers`,
//! polls it on the raw port through the watching transport (`pad_watch`), and the first traffic
//! on the pad after power-on is that driver's handshake. Input is the shared SDK and nothing else:
//! `Controllers` for the pad, `RadialStick` (the launcher's stick-nav fold, same radii) for the
//! left stick as d-pad, `PadTracker` for edges and the cursor's repeat. The watching transport,
//! the readout and the lab's variants are test instruments and are labelled so on screen.
//!
//! The pad lab needs no input, because a pad that cannot navigate must still be testable: it
//! cycles through variants of how the pad is polled, `PHASE_FRAMES` each, in the order the pad
//! console analysis asked for (see `rec::LAB_NAMES`). Variants that change only what the host
//! does come first; the ones that send the pad commands follow, each with the shipped path polled
//! for a phase afterwards.

use crate::pad_record::{self as rec, LabResult, PollNote, PowerOn, LAB_MAX};
use crate::pad_survey::{
    self, cfg, frame, rect, transact, txn_ok, wait_ticks, Entry, L, DIM, GOLD,
    GOOD, INK, PROFILES,
};
use crate::pad_watch::{mode_code, mode_name, reply_len, Watch};
use crate::ui;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::controller_port::Port;
use psx_io::irq;
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_pad::{button, AnalogRequirement, Controllers, PadState, PadTracker, RadialStick};
use psx_rt::{interrupts, tty};

/// Frames of no input (no button, stick at rest) before the survey starts by itself: 70 seconds,
/// long enough for the whole lab and a few seconds to read its last line.
const IDLE_ENTRY_FRAMES: u32 = 600 * 60;
/// Frames of one lab phase: 8 seconds.
const PHASE_FRAMES: u32 = 8 * 60;
/// Root counter 1 mode: count HBlanks (63.6 us), for the time `/CS` was held low.
const HBLANK_CLOCK: u16 = 0x0100;
/// Microseconds in one HBlank, times 100.
const HBLANK_US_X100: u32 = 6356;
/// Port 2 is polled every this many frames, only in the phase that releases the select anyway
/// and after the lab.
const P2_EVERY: u32 = 10;

/// Rows of the menu. Row 0 is pinned: `make hwtest-run` selects it by firing CROSS at a fixed
/// tick with the cursor still at its boot position.
const ROWS: [&str; 11] = [
    "RUN HARDWARE TEST",
    "GPU DONE SIGNAL",
    "GPU DONE FIXES",
    "GPU DONE FILLS",
    "RADIUS+NODE SWEEP",
    "CONTROLLER TEST",
    "CONTROLLER SURVEY",
    "MEMORY CARD RISK",
    "DUALSHOCK MOTORS",
    "LAST CAPTURE",
    "POWER-ON RECORD",
];
/// The row that starts the survey.
pub(crate) const ROW_SURVEY: u8 = 6;
/// The row that shows the power-on record's pages.
pub(crate) const ROW_POWER_ON: u8 = 10;

static mut POWER: PowerOn = PowerOn::EMPTY;

/// The power-on record as collected so far.
pub(crate) fn power_on() -> &'static PowerOn {
    // SAFETY: single thread; written only by `execute`, which does not run while a reader does.
    unsafe { &*core::ptr::addr_of!(POWER) }
}

fn power_mut() -> &'static mut PowerOn {
    // SAFETY: single thread; no reference outlives the call that takes it.
    unsafe { &mut *core::ptr::addr_of_mut!(POWER) }
}

/// Everything one menu session holds.
struct Menu {
    pads: Controllers,
    watch: Watch,
    stick: RadialStick<64, 32>,
    tracker: PadTracker,
    /// Port 2 has its own driver: sharing one would let a port-2 poll move the select the
    /// port-1 driver holds open.
    pads2: Controllers,
    watch2: Watch,
    bus: ControllerPort,
    cursor: u8,
    // Latest poll of port 1.
    note: PollNote,
    hold: u16,
    state: PadState,
    stick_fold_prev: u8,
    first_id: u8,
    polled: bool,
    // Port 2.
    p2: PollNote,
    p2_polls: u32,
    // Phase bookkeeping.
    cur: LabResult,
    last_done: LabResult,
    have_last: bool,
    late: u32,
    have_capture: bool,
    first: bool,
    iters: u32,
}

fn fold_code(bits: u16) -> u8 {
    u8::from(bits & button::UP != 0)
        | u8::from(bits & button::DOWN != 0) << 1
        | u8::from(bits & button::LEFT != 0) << 2
        | u8::from(bits & button::RIGHT != 0) << 3
}

impl Menu {
    /// One poll of port 1 through the shared SDK path, with the stick folded as the launcher does.
    #[inline(never)]
    fn poll_p1(&mut self, lab: u8) -> u16 {
        self.watch.begin_call();
        let state = *self.pads.poll(&mut self.watch, Port::One);
        let (mut note, hold) = self.watch.end_call(lab);
        // The launcher's fold: only while the mode reports sticks, else the stick state resets.
        let before = self.stick_fold_prev;
        let (bits, why) = if !state.mode.is_connected() {
            self.stick = RadialStick::new();
            (0, 3)
        } else if !state.mode.has_sticks() {
            self.stick = RadialStick::new();
            (0, 2)
        } else {
            let (x, y) = state.sticks.left_centered();
            let bits = self.stick.update(x, y);
            (bits, if bits != 0 { if before != 0 { 4 } else { 0 } } else { 1 })
        };
        self.stick_fold_prev = fold_code(bits);
        note.mode = mode_code(state.mode);
        note.flags = u8::from(state.is_connected())
            | u8::from(self.pads.analog(Port::One) == AnalogRequirement::Analog) << 1
            | u8::from(state.mode.has_sticks()) << 2;
        note.buttons = state.buttons.bits();
        note.sticks = [
            state.sticks.right_x,
            state.sticks.right_y,
            state.sticks.left_x,
            state.sticks.left_y,
        ];
        note.fold = fold_code(bits);
        note.why = why;
        self.state = state;
        self.note = note;
        self.hold = hold;
        if !self.polled {
            self.polled = true;
            self.first_id = note.first_id;
        }
        // Every poll of the first 600 goes into the power-on record.
        if self.first {
            power_mut().note(note, hold);
        }
        // The cur phase.
        let c = &mut self.cur;
        c.polls = c.polls.saturating_add(1);
        if note.class == 0 {
            c.ok = c.ok.saturating_add(1);
        } else {
            c.bad = c.bad.saturating_add(1);
        }
        if note.first_class != 0 {
            c.first_bad = c.first_bad.saturating_add(1);
        }
        c.btn_or |= note.buttons;
        c.last_id = note.id;
        c.last_mode = note.mode;
        c.last_flags = note.flags;
        if note.fold != 0 {
            c.fold_hits = c.fold_hits.saturating_add(1);
        }
        bits
    }

    /// One poll of port 2 (own driver).
    #[inline(never)]
    fn poll_p2(&mut self) {
        self.watch2.begin_call();
        let state = *self.pads2.poll(&mut self.watch2, Port::Two);
        let (mut note, _) = self.watch2.end_call(0xFF);
        note.mode = mode_code(state.mode);
        note.flags = u8::from(state.is_connected())
            | u8::from(self.pads2.analog(Port::Two) == AnalogRequirement::Analog) << 1;
        note.buttons = state.buttons.bits();
        self.p2 = note;
        self.p2_polls += 1;
    }

    /// Raw commands at LONG spacing on port 1, each transaction its own select: what the survey's
    /// raw sweep does. Counts into the phase result; keeps the reply to 0x45 if there was one.
    #[inline(never)]
    fn raw_sequence(&mut self, steps: &[[u8; 9]]) {
        for step in steps {
            let t = transact(&mut self.bus, Port::One, step, PROFILES[3], 0, 3, 0);
            self.cur.raw_total = self.cur.raw_total.saturating_add(1);
            if txn_ok(&t) {
                self.cur.raw_ok = self.cur.raw_ok.saturating_add(1);
            }
            if step[1] == 0x45 && t.got >= 9 {
                self.cur.status45.copy_from_slice(&t.rx[3..9]);
                self.cur.status45_set = 1;
            }
            wait_ticks(&mut self.bus, PROFILES[3].between);
            crate::bounds::heartbeat();
        }
    }
}

const ENTER: [u8; 9] = cfg(0x43, 0x01, 0x00);
const EXIT: [u8; 9] = cfg(0x43, 0x00, 0x00);

/// What phase `p` sends the pad before its polls start.
fn phase_start(m: &mut Menu, p: usize) {
    // Variant 2 alone leaves `/CS` high between polls.
    m.watch.release_between = p == 1;
    match p {
        // Variant 5: a driver that has never seen the pad.
        4 => m.pads = Controllers::new(),
        5 => m.raw_sequence(&[
            ENTER,
            [0x01, 0x4D, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF],
            EXIT,
        ]),
        6 => m.raw_sequence(&[
            ENTER,
            cfg(0x45, 0, 0),
            cfg(0x46, 0, 0),
            cfg(0x46, 1, 0),
            cfg(0x47, 0, 0),
            cfg(0x4C, 0, 0),
            cfg(0x4C, 1, 0),
            EXIT,
        ]),
        7 => m.raw_sequence(&[ENTER, cfg(0x44, 0x01, 0x03), EXIT]),
        _ => {}
    }
}

/// Frames between polls in phase `p`.
fn poll_every(p: usize) -> u32 {
    match p {
        2 => 2,
        3 => 4,
        _ => 1,
    }
}

/// Close lab phase `p`: a result line in the record.
fn phase_end(m: &mut Menu, p: usize, frames: u32) {
    let mut r = m.cur;
    r.phase = p as u8;
    r.frames = frames.min(0xFFFF) as u16;
    {
        let mut l = L::new();
        l.s("lab phase ").u(p as u32).s(" frames ").u(frames).s(" iters ").u(m.iters).s(" polls ").u(u32::from(m.cur.polls)).s(" late ").u(m.late);
        tty::println(l.as_str());
        m.iters = 0;
    }
    // After the shipped handshake of phase 5 (index 4), ask the pad its status once, as the
    // pad console's candidate D wants. It is the status the SDK's own handshake left.
    if p == 4 {
        let saved = m.cur;
        m.cur = LabResult::default();
        m.raw_sequence(&[ENTER, cfg(0x45, 0, 0), EXIT]);
        r.status45 = m.cur.status45;
        r.status45_set = m.cur.status45_set;
        r.raw_ok = m.cur.raw_ok;
        r.raw_total = m.cur.raw_total;
        m.cur = saved;
    }
    if m.first {
        power_mut().lab_result(r);
    }
    m.last_done = r;
    m.have_last = true;
    m.cur = LabResult::default();
}

// --------------------------------------------------------------------------- drawing

/// Screen bands, one drawn per frame so no frame spends more than a fraction of itself drawing
/// (the poll is on the frame clock, and a full redraw took longer than a frame): rows from, to.
const BANDS: [(i16, i16); 8] = [
    (0, 29),
    (30, 75),
    (76, 111),
    (112, 138),
    (139, 165),
    (166, 192),
    (193, 219),
    (220, 239),
];
/// The band being drawn.
static mut BAND: (i16, i16) = (0, 239);

/// Draw `s` at `x`, `y`, cut so that it ends before x 320, if its row is in the band being drawn.
fn txt(font: &FontAtlas, x: i16, y: i16, s: &str, c: (u8, u8, u8)) {
    // SAFETY: single thread; plain static.
    let (lo, hi) = unsafe { BAND };
    if y < lo || y > hi {
        return;
    }
    let room = ((320 - x) / 8).max(0) as usize;
    let s = if s.len() > room { &s[..room] } else { s };
    if !s.is_empty() {
        font.draw_text(x, y, s, c);
    }
}

/// Draw band `step` of the screen into the buffer the display is not showing; the last band
/// queues the flip. A screen therefore takes `BANDS.len()` frames and is never seen half drawn.
#[inline(never)]
fn paint_step(font: &FontAtlas, v: &View, step: usize) {
    if step == 0 {
        // The buffer to draw into is the one the display is leaving: the flip queued last time
        // must have been applied. Bounded.
        let mut spins = 0u32;
        while psx_rt::present::is_flip_pending() && spins < 2_000_000 {
            spins += 1;
        }
    }
    // SAFETY: single thread; plain statics.
    let back = 240 - unsafe { pad_survey::FRONT };
    // SAFETY: as above.
    unsafe { BAND = BANDS[step] };
    {
        probe_gpu!(gpu);
        psx_io::gpu::wait_command_ready();
        gpu.set_draw_area((0, back as u16), (319, back as u16 + 239));
        gpu.set_draw_offset((0, back));
        draw(font, v);
        psx_io::gpu::wait_command_ready();
        gpu.set_draw_offset((0, 0));
    }
    if step + 1 == BANDS.len() {
        interrupts::queue_display_control_at_vblank(0x0500_0000 | ((back as u32) << 10));
        // SAFETY: as above.
        unsafe { pad_survey::FRONT = back };
    }
}

/// Class names as the screen has room for them (the record's are `rec::NOTE_CLASS_NAMES`).
const CLASS: [&str; 6] = ["OK", "SHORT", "GARBLED", "NO PAD", "NO EXCH", "ADDR NAK"];

fn hex_list(l: &mut L, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            l.s(" ");
        }
        l.hex(u32::from(*b), 2);
    }
}

fn class_colour(class: u8) -> (u8, u8, u8) {
    if class == 0 {
        GOOD
    } else {
        (255, 120, 96)
    }
}

/// How many acknowledgements a complete poll of this ID has: every byte but the last.
fn acks_needed(id: u8) -> u32 {
    if id == 0xFF {
        8
    } else {
        reply_len(id).min(9) as u32 - 1
    }
}

struct View {
    cursor: u8,
    note: PollNote,
    hold: u16,
    counters: rec::Counters,
    first_id: u8,
    late: u32,
    p2: PollNote,
    p2_polls: u32,
    phase: Option<usize>,
    phase_left: u32,
    lab_left: u32,
    cur: LabResult,
    last: Option<LabResult>,
    idle_left: u32,
    have_capture: bool,
}

#[inline(never)]
fn draw(font: &FontAtlas, v: &View) {
    // SAFETY: single thread; plain static.
    let (lo, hi) = unsafe { BAND };
    if hi >= lo {
        rect(0, lo, 320, (hi - lo + 1) as u16, (6, 8, 18));
    }
    let n = &v.note;
    txt(font, 4, 2, "PS1 HARDWARE TESTS", INK);
    txt(font, 320 - 4 - crate::SUITE_VERSION.len() as i16 * 8, 2, crate::SUITE_VERSION, DIM);
    txt(font, 4, 11, "SDK 6E25F2AE + PAD FIX (LANE PAD-AB)", DIM);
    for (i, label) in ROWS.iter().enumerate() {
        let (col, row) = (i as i16 / 6, i as i16 % 6);
        let (x, y) = (4 + col * 160, 21 + row * 9);
        let sel = i as u8 == v.cursor;
        let c = if sel { GOLD } else { (176, 190, 210) };
        if sel {
            txt(font, x, y, ">", GOLD);
        }
        txt(font, x + 10, y, label, c);
    }
    txt(font, 4, 76, "FIXED SDK PATH (WATCHED), STICK NAV", GOLD);
    let mut l = L::new();
    l.s("P1 ").s(CLASS[usize::from(n.class).min(5)]);
    l.s("  ID ").hex(u32::from(n.id), 2);
    l.s("  ACKED ").u(u32::from(n.acked)).s(" OF ").u(acks_needed(n.id));
    txt(font, 4, 85, l.as_str(), class_colour(n.class));
    let mut l = L::new();
    l.s("1ST TRY ").s(CLASS[usize::from(n.first_class).min(5)]);
    l.s(" ID ").hex(u32::from(n.first_id), 2);
    if n.first_died != 0xFF {
        l.s(" DIED ").u(u32::from(n.first_died));
    }
    l.s(" TRIES ").u(u32::from(n.tries));
    txt(font, 4, 94, l.as_str(), class_colour(n.first_class));
    let mut l = L::new();
    l.s("RX ");
    hex_list(&mut l, &n.rx[..usize::from(n.n).min(9)]);
    txt(font, 4, 103, l.as_str(), INK);
    let mut l = L::new();
    if n.fresh & 1 != 0 {
        l.s("FRESH SELECT (NOT HELD) ");
    } else {
        l.s("HELD SELECT ");
    }
    l.s("/CS LOW ").u(u32::from(v.hold) * HBLANK_US_X100 / 100).s(" US");
    txt(font, 4, 112, l.as_str(), INK);
    let mut l = L::new();
    l.s("SDK ").s(if n.flags & 1 != 0 { "CONNECTED " } else { "NO PAD " });
    l.s(mode_name(n.mode)).s(if n.flags & 2 != 0 { " LOCKED" } else { " UNLOCKED" });
    txt(font, 4, 121, l.as_str(), INK);
    let mut l = L::new();
    l.s("BUTTONS ").hex(u32::from(n.buttons), 4);
    l.s("  STICK RX ").hex(u32::from(n.sticks[0]), 2).s(" RY ").hex(u32::from(n.sticks[1]), 2);
    txt(font, 4, 130, l.as_str(), INK);
    let mut l = L::new();
    l.s("WIRE ");
    if n.n >= 9 {
        hex_list(&mut l, &n.rx[5..9]);
    } else {
        l.s("--");
    }
    l.s("  SDK LX ").hex(u32::from(n.sticks[2]), 2).s(" LY ").hex(u32::from(n.sticks[3]), 2);
    txt(font, 4, 139, l.as_str(), INK);
    let mut l = L::new();
    l.s("FOLD ");
    if n.fold == 0 {
        l.s("NONE");
    } else {
        for (bit, name) in [(1, "UP "), (2, "DOWN "), (4, "LEFT "), (8, "RIGHT ")] {
            if n.fold & bit != 0 {
                l.s(name);
            }
        }
    }
    l.s(": ").s(rec::FOLD_WHY[usize::from(n.why).min(5)]);
    txt(font, 4, 148, l.as_str(), if n.fold != 0 { GOOD } else { INK });
    let c = &v.counters;
    let mut l = L::new();
    l.s("OK ").u(c.ok).s(" BAD ").u(c.bad).s(" NAK ").u(c.nak).s(" RETRY ").u(c.retry);
    l.s(" LATE ").u(v.late);
    txt(font, 4, 157, l.as_str(), INK);
    let mut l = L::new();
    l.s("HS ").u(c.handshakes).s(" CFG ").u(c.cfg).s(" EXIT ").u(c.exits).s(" EXIT-F3 ").u(c.exits_f3);
    l.s(" ID0 ").hex(u32::from(v.first_id), 2);
    txt(font, 4, 166, l.as_str(), INK);
    let mut l = L::new();
    if v.p2_polls == 0 {
        l.s("P2 NOT POLLED YET (PHASE 2 AND AFTER 64 S)");
    } else {
        l.s("P2 ").s(CLASS[usize::from(v.p2.class).min(5)]);
        l.s(" ID ").hex(u32::from(v.p2.id), 2).s(" ").s(mode_name(v.p2.mode));
        l.s(" BTN ").hex(u32::from(v.p2.buttons), 4);
    }
    txt(font, 4, 175, l.as_str(), DIM);
    txt(font, 4, 184, "PAD LAB, AN EXPERIMENT NOT THE SDK PATH", GOLD);
    let mut l = L::new();
    match v.phase {
        Some(p) => {
            l.s(rec::LAB_NAMES[p]);
        }
        None => {
            l.s("A/B DONE, FIXED SDK PATH");
        }
    }
    txt(font, 4, 193, l.as_str(), INK);
    let mut l = L::new();
    if v.phase.is_some() {
        l.s("PHASE ").u(v.phase_left / 60 + 1).s(" S LEFT, LAB ").u(v.lab_left / 60 + 1).s(" S");
    }
    l.s("  OK ").u(u32::from(v.cur.ok)).s(" BAD ").u(u32::from(v.cur.bad));
    txt(font, 4, 202, l.as_str(), INK);
    let mut l = L::new();
    l.s("1ST TRY BAD ").u(u32::from(v.cur.first_bad)).s(" BTN SEEN ").hex(u32::from(v.cur.btn_or), 4);
    l.s(" FOLD ").u(u32::from(v.cur.fold_hits));
    txt(font, 4, 211, l.as_str(), INK);
    let mut l = L::new();
    match v.last {
        Some(r) => {
            l.s("LAST ").u(u32::from(r.phase) + 1).s(" OK ").u(u32::from(r.ok)).s(" BAD ").u(u32::from(r.bad));
            l.s(" 1ST ").u(u32::from(r.first_bad)).s(" BTN ").hex(u32::from(r.btn_or), 4);
            if r.status45_set != 0 {
                l.s(" S45 ");
                hex_list(&mut l, &r.status45);
            }
        }
        None => {
            l.s("THE PAD A/B RAN BEFORE THIS MENU");
        }
    }
    txt(font, 4, 220, l.as_str(), DIM);
    let mut l = L::new();
    l.s("NO INPUT IN ").u(v.idle_left / 60 + 1).s(" S: SURVEY. UP/DOWN, X");
    txt(font, 4, 229, l.as_str(), GOLD);
}

// --------------------------------------------------------------------------- the menu

/// Run the menu until a row is chosen and return it. Rows `ROW_SURVEY` and `ROW_POWER_ON` never
/// return: the survey and the power-on record's pages take the machine. `first` is true for the
/// session before the engine started (the pad is fresh from power); a later session, entered from
/// a test that finished, keeps the lab but does not write the power-on record again.
pub(crate) fn execute(first: bool, have_capture: bool, baseline_irq: u32, baseline_dpcr: u32) -> u8 {
    interrupts::install_vblank_counter();
    if !first {
        // In the frame loop the mask the engine runs with is what comes back; before the engine
        // the one `install_vblank_counter` made (VBlank on) is what the menu needs.
        irq::set_mask(baseline_irq);
    }
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let _ = Gpu::new(
        crate::probe_gpu_dma(),
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    // SAFETY: single thread; the display shows the first buffer after the GPU is set up.
    unsafe { pad_survey::FRONT = 0 };
    let font = ui::upload_font();
    let mut m = Menu {
        pads: Controllers::new(),
        watch: Watch::new(),
        stick: RadialStick::new(),
        tracker: PadTracker::new(),
        pads2: Controllers::new(),
        watch2: Watch::new(),
        // SAFETY: a token is a logic guard; nothing else drives SIO0 while the menu runs.
        bus: unsafe { ControllerPort::steal() },
        cursor: 0,
        note: PollNote::default(),
        hold: 0,
        state: PadState::NONE,
        stick_fold_prev: 0,
        first_id: 0xFF,
        polled: false,
        p2: PollNote::default(),
        p2_polls: 0,
        cur: LabResult::default(),
        last_done: LabResult::default(),
        have_last: false,
        late: 0,
        have_capture,
        first,
        iters: 0,
    };
    tty::println("hardware-tests: pad menu ready");
    // The first screen goes up before the first poll: that poll is the handshake from power, a
    // few frames long, and the screen should not be empty while it runs.
    let mut v = View {
        cursor: 0,
        note: PollNote::default(),
        hold: 0,
        counters: m.watch.counters,
        first_id: 0xFF,
        late: 0,
        p2: PollNote::default(),
        p2_polls: 0,
        phase: Some(0),
        phase_left: PHASE_FRAMES,
        lab_left: PHASE_FRAMES * LAB_MAX as u32,
        cur: LabResult::default(),
        last: None,
        idle_left: IDLE_ENTRY_FRAMES,
        have_capture,
    };
    v.note.class = 4;
    for band in 0..BANDS.len() {
        paint_step(&font, &v, band);
    }
    let bits = m.poll_p1(0);
    m.tracker.update(m.state.buttons.bits() | bits);
    // A button held at power-on is not a press.
    m.tracker.settle();
    let start_vb = interrupts::vblank_count();
    let mut last_vb = start_vb;
    // v2.23: the A/B ran before the menu; the v2.22 lab is not repeated, the menu is for use.
    let mut phase = LAB_MAX;
    let mut phase_vb = start_vb;
    let mut idle = 0u32;
    let mut lab_done = true;
    let mut band = 0usize;
    loop {
        frame();
        m.iters += 1;
        let now = interrupts::vblank_count();
        let elapsed = now.wrapping_sub(last_vb);
        last_vb = now;
        if elapsed > 1 {
            m.late += elapsed - 1;
        }
        let since = now.wrapping_sub(start_vb);
        // Phase changes.
        if !lab_done {
            let p = ((since / PHASE_FRAMES) as usize).min(LAB_MAX);
            while phase < p {
                phase_end(&mut m, phase, now.wrapping_sub(phase_vb));
                phase += 1;
                phase_vb = now;
                if phase < LAB_MAX {
                    phase_start(&mut m, phase);
                } else {
                    lab_done = true;
                    m.watch.release_between = false;
                }
            }
        }
        let p = phase.min(LAB_MAX - 1);
        let every = if lab_done { 1 } else { poll_every(p) };
        let lab = if lab_done { 0xFF } else { p as u8 };
        if now % every == 0 {
            let bits = m.poll_p1(lab);
            m.tracker.update(m.state.buttons.bits() | bits);
            // Any button or a pushed stick is activity.
            let active = m.state.buttons.bits() | bits != 0;
            idle = if active { 0 } else { idle + elapsed.max(1) };
            if m.tracker.repeats(button::DOWN, 15, 4) {
                m.cursor = (m.cursor + 1) % ROWS.len() as u8;
            }
            if m.tracker.repeats(button::UP, 15, 4) {
                m.cursor = (m.cursor + ROWS.len() as u8 - 1) % ROWS.len() as u8;
            }
            if m.tracker.just_pressed(button::CROSS) {
                let row = m.cursor;
                if row != 9 || m.have_capture {
                    return finish(m, row, baseline_dpcr, phase, now.wrapping_sub(phase_vb));
                }
            }
        } else {
            idle += elapsed.max(1);
        }
        if (phase == 1 || lab_done) && now % P2_EVERY == 0 {
            m.poll_p2();
        }
        if idle >= IDLE_ENTRY_FRAMES {
            let _ = finish(m, ROW_SURVEY, baseline_dpcr, phase, now.wrapping_sub(phase_vb));
            unreachable!();
        }
        // One band of the screen a frame, so the poll cadence is the frame cadence. The view is
        // taken at the first band and the screen drawn from it, whole.
        if band == 0 {
            v.cursor = m.cursor;
            v.note = m.note;
            v.hold = m.hold;
            v.counters = m.watch.counters;
            v.first_id = m.first_id;
            v.late = m.late;
            v.p2 = m.p2;
            v.p2_polls = m.p2_polls;
            v.phase = if lab_done { None } else { Some(p) };
            v.phase_left = PHASE_FRAMES - (since % PHASE_FRAMES).min(PHASE_FRAMES);
            v.lab_left = (PHASE_FRAMES * LAB_MAX as u32).saturating_sub(since);
            v.cur = m.cur;
            v.last = if m.have_last { Some(m.last_done) } else { None };
            v.idle_left = IDLE_ENTRY_FRAMES.saturating_sub(idle);
        }
        // One frame after the last band draws nothing: the flip it queued is applied by then.
        if band < BANDS.len() {
            paint_step(&font, &v, band);
        }
        band = (band + 1) % (BANDS.len() + 1);
    }
}

/// Close the power-on record and leave: the survey and the record's pages take the machine, any
/// other row goes back to the caller.
fn finish(mut m: Menu, row: u8, baseline_dpcr: u32, phase: usize, frames: u32) -> u8 {
    let p = power_mut();
    if m.first {
        p.counters = m.watch.counters;
        p.frames = interrupts::vblank_count();
    }
    // The open phase is closed as it stands, so the record has a line for it.
    if m.first && phase < LAB_MAX && usize::from(p.lab_n) == phase {
        let mut r = m.cur;
        r.phase = phase as u8;
        r.frames = frames.min(0xFFFF) as u16;
        p.lab_result(r);
    }
    match row {
        // The survey runs the frame loop itself and needs VBlank on: the mask as it stands now
        // (the engine's, or before it the one the menu made).
        ROW_SURVEY => pad_survey::execute(Entry::Survey(m.pads), irq::mask(), baseline_dpcr),
        ROW_POWER_ON => pad_survey::execute(Entry::PowerOnPages, irq::mask(), baseline_dpcr),
        _ => row,
    }
}
