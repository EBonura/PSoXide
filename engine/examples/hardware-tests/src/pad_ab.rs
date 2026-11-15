// SPDX-License-Identifier: GPL-2.0-or-later
//! The pad A/B, before the menu: the same pad polled four ways from power-on, no input needed.
//!
//! P1 PLAIN is what the SDK did when the launcher of demo disc v0.56 failed on the SCPH-110: the
//! select left open after every poll, nothing watching, no wait with `/CS` high. P2 FIXED is the
//! SDK path as it is now (select at the start of each poll, `/CS` high between polls). P3 WATCHED
//! is the held select through the watching transport (what the v2.21 survey and the v2.22 menu
//! did, and the SCPH-110 worked). P4 is P1 again, to see whether the order mattered.
//!
//! P1 and P3 are test instruments: the SDK has no way to hold a select any more, so the instrument
//! does it itself after each poll returns, at the time the old driver did (`Plain`, `Watch`).
//! Every phase is a fresh `Controllers` with the port reset and `/CS` high for a second before it.
//! The counters are `Controllers`' own (feature `counters`): polls, failed, first-try failed,
//! handshakes. They touch no wire.

use crate::pad_survey::{self, frame, rect, L, DIM, GOLD, GOOD, INK};
use crate::pad_watch::{mode_code, mode_name, Watch};
use crate::ui;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::controller_port::{Port, Transport};
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_pad::{Controllers, PadState, RadialStick};
use psx_rt::{interrupts, tty};

/// Frames of one phase: 8 seconds.
const PHASE_FRAMES: u32 = 8 * 60;
/// Frames with `/CS` high and no poll before a phase.
const GAP_FRAMES: u32 = 60;
/// Frames the verdicts are held for the photo: 45 seconds.
const HOLD_FRAMES: u32 = 45 * 60;
/// Frames of the stick check: 20 seconds.
const STICK_FRAMES: u32 = 20 * 60;
/// Cycles `/CS` was high between a poll and the select the old driver left open, as measured in
/// the emulator on the burned v0.56 launcher: 386 from the deselect write to the select's last
/// write. The wait covers all but the select's own writes and the clock reads.
const SHIPPED_CS_HIGH: u16 = 306;

/// The four phases: label, what it is.
const NAMES: [(&str, &str); 4] = [
    ("P1 PLAIN", "SHIPPED v0.56 TIMING"),
    ("P2 FIXED", "SDK PATH, /CS HIGH"),
    ("P3 WATCHED", "HELD SELECT, WATCHER"),
    ("P4 PLAIN", "AGAIN"),
];

/// The port as the SDK drove it in demo disc v0.56: no wait with `/CS` high before a retry or
/// the handshake, and a select left open after each successful poll (done in `finish`).
struct Plain {
    port: ControllerPort,
    opened: Option<u16>,
    socket: Port,
    position: u8,
    command: u8,
}

impl Transport for Plain {
    fn status(&mut self) -> u32 {
        self.port.status()
    }
    fn receive(&mut self) -> u8 {
        self.port.receive()
    }
    fn transmit(&mut self, byte: u8) {
        if self.position == 1 {
            self.command = byte;
        }
        self.position = self.position.saturating_add(1);
        self.port.transmit(byte);
    }
    fn set_mode(&mut self, value: u16) {
        self.port.set_mode(value);
    }
    fn set_baud(&mut self, value: u16) {
        self.port.set_baud(value);
    }
    fn set_control(&mut self, value: u16) {
        self.port.set_control(value);
    }
    fn control(&mut self) -> u16 {
        self.port.control()
    }
    fn clock(&mut self) -> u16 {
        self.port.clock()
    }
    fn mask_interrupts(&mut self) -> bool {
        self.port.mask_interrupts()
    }
    fn restore_interrupts(&mut self, were_enabled: bool) {
        self.port.restore_interrupts(were_enabled);
    }
    /// The shipped driver waited for nothing here.
    fn cs_high_gap(&mut self) {}
    /// The first try of a poll starts from the select the last poll left open, as it did.
    fn begin_pad(&mut self, port: Port, _opened: Option<u16>) -> bool {
        self.socket = port;
        let held = self.opened.take();
        self.port.begin_pad(port, held)
    }
    /// The old driver left the next poll's select open once a poll succeeded, a few hundred cycles
    /// after the deselect. It did that after the poll returned; here it is done in `finish`, from
    /// the deselect itself, so the time `/CS` is high is the one measured on the shipped launcher
    /// whatever the code in between costs.
    fn finish(&mut self, succeeded: bool) {
        let poll = succeeded && self.command == 0x42;
        self.position = 0;
        self.command = 0;
        self.port.finish(succeeded);
        if poll {
            let at = self.port.clock();
            while self.port.clock().wrapping_sub(at) < SHIPPED_CS_HIGH {}
            self.opened = Some(self.port.open(self.socket));
        }
    }
}

impl Plain {
    fn new() -> Self {
        Self {
            // SAFETY: a token is a logic guard; nothing else drives SIO0 while the A/B runs.
            port: unsafe { ControllerPort::steal() },
            opened: None,
            socket: Port::One,
            position: 0,
            command: 0,
        }
    }

}

/// What one phase counted.
#[derive(Clone, Copy)]
struct Phase {
    done: bool,
    polls: u16,
    failed: u16,
    first_failed: u16,
    handshakes: u16,
    mode: u8,
    buttons_seen: u16,
    sticks: [u8; 4],
}

impl Phase {
    const NEW: Phase = Phase {
        done: false,
        polls: 0,
        failed: 0,
        first_failed: 0,
        handshakes: 0,
        mode: 0,
        buttons_seen: 0,
        sticks: [0; 4],
    };
}

/// 0 alive, 1 marginal, 2 dead.
fn verdict(p: &Phase) -> u8 {
    if p.polls == 0 || u32::from(p.failed) * 2 > u32::from(p.polls) || p.handshakes > 3 {
        2
    } else if p.failed == 0 && p.first_failed == 0 && p.handshakes <= 1 {
        0
    } else {
        1
    }
}

const VERDICT: [&str; 3] = ["ALIVE", "MARGINAL", "DEAD"];
const VERDICT_COLOUR: [(u8, u8, u8); 3] = [GOOD, (255, 200, 80), (255, 104, 96)];

/// What the screen shows.
#[derive(Clone, Copy)]
struct View {
    phases: [Phase; 4],
    /// 0 to 3 a phase running or about to, 4 the held verdicts, 5 the stick check.
    stage: u8,
    left: u32,
    /// The stick check.
    mode: u8,
    sticks: [u8; 4],
    fold: u16,
    min: [u8; 4],
    max: [u8; 4],
}

/// Screen bands, one painted per frame so the poll cadence stays the frame cadence. Every band
/// is a whole number of text rows: a row never crosses a band edge (it would be cut).
const BANDS: [(i16, i16); 7] =
    [(0, 12), (13, 50), (51, 88), (89, 126), (127, 164), (165, 202), (203, 239)];
static mut BAND: (i16, i16) = (0, 239);

/// Row `k` of band `b`.
const fn row(b: usize, k: i16) -> i16 {
    BANDS[b].0 + 1 + 9 * k
}

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

/// A filled bar `h` rows high at `y`, drawn only in the band being painted.
fn bar(y: i16, h: i16, c: (u8, u8, u8)) {
    // SAFETY: single thread; plain static.
    let (lo, hi) = unsafe { BAND };
    let top = y.max(lo);
    let bottom = (y + h - 1).min(hi);
    if bottom >= top {
        rect(0, top, 320, (bottom - top + 1) as u16, c);
    }
}

fn hex_list(l: &mut L, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            l.s(" ");
        }
        l.hex(u32::from(*b), 2);
    }
}

/// What each phase is, as the screen has room for it.
const WHAT: [&str; 4] = ["SHIPPED v0.56", "SDK, /CS HIGH", "HELD+WATCHER", "SHIPPED AGAIN"];

#[inline(never)]
fn draw(font: &FontAtlas, v: &View) {
    // SAFETY: single thread; plain static.
    let (lo, hi) = unsafe { BAND };
    if hi >= lo {
        rect(0, lo, 320, (hi - lo + 1) as u16, (6, 8, 18));
    }
    txt(font, 4, 3, "PAD A/B", INK);
    txt(font, 320 - 4 - crate::SUITE_VERSION.len() as i16 * 8, 3, crate::SUITE_VERSION, DIM);
    if v.stage == 5 {
        draw_sticks(font, v);
        return;
    }
    for (i, p) in v.phases.iter().enumerate() {
        let b = i + 1;
        let y = row(b, 0);
        let mut l = L::new();
        l.s(NAMES[i].0);
        if p.done {
            let k = usize::from(verdict(p));
            bar(y - 1, 11, VERDICT_COLOUR[k]);
            l.s("  ").s(VERDICT[k]);
            txt(font, 4, y, l.as_str(), (6, 8, 18));
        } else if usize::from(v.stage) == i {
            l.s("  RUNNING ").u(v.left / 60 + 1).s(" S");
            txt(font, 4, y, l.as_str(), GOLD);
        } else {
            l.s("  WAITING");
            txt(font, 4, y, l.as_str(), DIM);
        }
        txt(font, 4 + 8 * 22, y, WHAT[i], if p.done { (6, 8, 18) } else { DIM });
        let mut l = L::new();
        l.s("POLLS ").u(u32::from(p.polls)).s(" FAILED ").u(u32::from(p.failed));
        l.s(" 1ST-TRY ").u(u32::from(p.first_failed)).s(" HS ").u(u32::from(p.handshakes));
        txt(font, 4, row(b, 1), l.as_str(), INK);
        let mut l = L::new();
        l.s(mode_name(p.mode)).s(" BTN ").hex(u32::from(p.buttons_seen), 4).s(" ST ");
        hex_list(&mut l, &p.sticks);
        txt(font, 4, row(b, 2), l.as_str(), INK);
    }
    let mut l = L::new();
    if v.stage == 4 {
        l.s("VERDICTS HELD ").u(v.left / 60 + 1).s(" S: PHOTO NOW");
        txt(font, 4, row(5, 0), l.as_str(), GOLD);
        txt(font, 4, row(5, 1), "THEN CIRCLE BOTH STICKS 20 S, THEN MENU", DIM);
    } else {
        txt(font, 4, row(5, 0), "HOLD R1 FROM POWER ON. NOTHING ELSE.", DIM);
    }
    txt(font, 4, row(5, 3), "ST = RX RY LX LY STICK BYTES", DIM);
    txt(font, 4, row(6, 0), "ALIVE: 0 FAILED 0 1ST-TRY 1 HANDSHAKE", DIM);
    txt(font, 4, row(6, 1), "MARGINAL: 1ST-TRY MISSES, RETRY OK", DIM);
    txt(font, 4, row(6, 2), "DEAD: MOST POLLS FAIL OR HS REPEATS", DIM);
    txt(font, 4, row(6, 3), "PHOTO THIS SCREEN FOR EACH PAD", GOLD);
}

fn draw_sticks(font: &FontAtlas, v: &View) {
    txt(font, 4, row(1, 0), "STICK CHECK: CIRCLE BOTH STICKS", GOLD);
    let mut l = L::new();
    l.s("LEFT ").u(v.left / 60 + 1).s(" S  MODE ").s(mode_name(v.mode));
    txt(font, 4, row(1, 1), l.as_str(), INK);
    let mut l = L::new();
    l.s("RIGHT X ").hex(u32::from(v.sticks[0]), 2).s("  Y ").hex(u32::from(v.sticks[1]), 2);
    txt(font, 4, row(1, 2), l.as_str(), INK);
    let mut l = L::new();
    l.s("LEFT  X ").hex(u32::from(v.sticks[2]), 2).s("  Y ").hex(u32::from(v.sticks[3]), 2);
    txt(font, 4, row(1, 3), l.as_str(), INK);
    let mut l = L::new();
    l.s("FOLD: ");
    if v.fold == 0 {
        l.s("NONE");
    } else {
        for (bit, name) in [
            (psx_pad::button::UP, "UP "),
            (psx_pad::button::DOWN, "DOWN "),
            (psx_pad::button::LEFT, "LEFT "),
            (psx_pad::button::RIGHT, "RIGHT "),
        ] {
            if v.fold & bit != 0 {
                l.s(name);
            }
        }
    }
    txt(font, 4, row(2, 0), l.as_str(), if v.fold != 0 { GOOD } else { INK });
    let mut l = L::new();
    l.s("MIN ");
    hex_list(&mut l, &v.min);
    txt(font, 4, row(2, 1), l.as_str(), DIM);
    let mut l = L::new();
    l.s("MAX ");
    hex_list(&mut l, &v.max);
    txt(font, 4, row(2, 2), l.as_str(), DIM);
    txt(font, 4, row(2, 3), "RX RY LX LY SINCE THE CHECK STARTED", DIM);
    txt(font, 4, row(3, 0), "VERDICTS, FOR THE PHOTO", GOLD);
    for (i, p) in v.phases.iter().enumerate() {
        let k = usize::from(verdict(p));
        let mut l = L::new();
        l.s(NAMES[i].0).s(" ").s(VERDICT[k]).s(" F ").u(u32::from(p.failed));
        l.s(" 1ST ").u(u32::from(p.first_failed)).s(" HS ").u(u32::from(p.handshakes));
        // Rows 1 to 3 of band 3 and row 0 of band 4.
        let (b, r) = if i < 3 { (3, i as i16 + 1) } else { (4, 0) };
        txt(font, 4, row(b, r), l.as_str(), VERDICT_COLOUR[k]);
    }
    txt(font, 4, row(5, 0), "SDK FOLD: PadState::menu_stick, STICKS", DIM);
    txt(font, 4, row(5, 1), "COUNT IN ANALOG OR CONFIG MODE.", DIM);
    txt(font, 4, row(6, 0), "THEN THE MENU, ON THE SAME FIXED PATH", GOLD);
}

/// One band of the screen; the last queues the flip.
#[inline(never)]
fn paint_step(font: &FontAtlas, v: &View, step: usize) {
    if step == 0 {
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

/// Which transport a phase polls through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Plain,
    Fixed,
    Watched,
}

const WAYS: [Way; 4] = [Way::Plain, Way::Fixed, Way::Watched, Way::Plain];

fn reset_port(bus: &mut ControllerPort) {
    bus.reset();
    bus.deselect();
}

/// Run the whole A/B, the stick check and leave the screen to the menu.
pub(crate) fn execute() {
    interrupts::install_vblank_counter();
    timers::set_mode(Timer::Timer1, 0x0100);
    timers::set_counter(Timer::Timer1, 0);
    let _ = Gpu::new(
        crate::probe_gpu_dma(),
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    // SAFETY: single thread; the display shows the first buffer after the GPU is set up.
    unsafe { pad_survey::FRONT = 0 };
    let font = ui::upload_font();
    let mut v = View {
        phases: [Phase::NEW; 4],
        stage: 0,
        left: PHASE_FRAMES,
        mode: 0,
        sticks: [0x80; 4],
        fold: 0,
        min: [0xFF; 4],
        max: [0; 4],
    };
    tty::println("hardware-tests: pad A/B");
    let mut band = 0usize;
    // One frame: wait for the vblank, then a band of the screen. The poll comes first so its
    // cadence is the frame cadence.
    let mut screen = |v: &View| {
        if band < BANDS.len() {
            paint_step(&font, v, band);
        }
        band = (band + 1) % (BANDS.len() + 1);
    };
    // SAFETY: a token is a logic guard; nothing else drives SIO0 while the A/B runs.
    let mut bare = unsafe { ControllerPort::steal() };
    for (i, way) in WAYS.iter().enumerate() {
        v.stage = i as u8;
        v.left = GAP_FRAMES + PHASE_FRAMES;
        reset_port(&mut bare);
        let mut pads = Controllers::new();
        let mut plain = Plain::new();
        let mut watch = Watch::new();
        watch.hold_polls = true;
        let mut p = Phase::NEW;
        v.phases[i] = p;
        for f in 0..GAP_FRAMES + PHASE_FRAMES {
            frame();
            v.left = GAP_FRAMES + PHASE_FRAMES - f;
            if f >= GAP_FRAMES {
                let state: PadState = match way {
                    Way::Plain => {
                        *pads.poll(&mut plain, Port::One)
                    }
                    Way::Fixed => *pads.poll(&mut bare, Port::One),
                    Way::Watched => {
                        watch.begin_call();
                        let s = *pads.poll(&mut watch, Port::One);
                        let _ = watch.end_call(i as u8);
                        s
                    }
                };
                let c = pads.counters(Port::One);
                p.polls = c.polls;
                p.failed = c.failed;
                p.first_failed = c.first_try_failed;
                p.handshakes = c.handshakes;
                p.mode = mode_code(state.mode);
                p.buttons_seen |= state.buttons.bits();
                p.sticks = [
                    state.sticks.right_x,
                    state.sticks.right_y,
                    state.sticks.left_x,
                    state.sticks.left_y,
                ];
                v.phases[i] = p;
            }
            screen(&v);
        }
        p.done = true;
        v.phases[i] = p;
        reset_port(&mut bare);
        let mut l = L::new();
        l.s("pad A/B ").s(NAMES[i].0).s(" ").s(VERDICT[usize::from(verdict(&p))]);
        l.s(" polls ").u(u32::from(p.polls)).s(" failed ").u(u32::from(p.failed));
        l.s(" first ").u(u32::from(p.first_failed)).s(" hs ").u(u32::from(p.handshakes));
        tty::println(l.as_str());
    }
    // The verdicts, held for the photo.
    v.stage = 4;
    for f in 0..HOLD_FRAMES {
        frame();
        v.left = HOLD_FRAMES - f;
        screen(&v);
    }
    // The stick check, through the fixed path with the menu's fold.
    v.stage = 5;
    reset_port(&mut bare);
    let mut pads = Controllers::new();
    let mut fold = RadialStick::<64, 32>::new();
    for f in 0..STICK_FRAMES {
        frame();
        v.left = STICK_FRAMES - f;
        let s = *pads.poll(&mut bare, Port::One);
        v.mode = mode_code(s.mode);
        v.fold = fold.update_pad(&s);
        v.sticks = [s.sticks.right_x, s.sticks.right_y, s.sticks.left_x, s.sticks.left_y];
        if s.mode.has_sticks() {
            for k in 0..4 {
                v.min[k] = v.min[k].min(v.sticks[k]);
                v.max[k] = v.max[k].max(v.sticks[k]);
            }
        }
        screen(&v);
    }
    reset_port(&mut bare);
    tty::println("hardware-tests: pad A/B done");
}
