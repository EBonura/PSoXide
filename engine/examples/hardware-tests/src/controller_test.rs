//! Friendly two-port controller diagnostic.
//!
//! This intentionally lives beside, rather than replacing, the low-level SIO
//! controller probe in `main.rs`. The probe measures serial timing for the
//! conformance suite; this screen answers the operator-facing questions: which
//! ports have pads, which buttons have worked, and whether an untouched analog
//! stick rests far enough from 0x80 to indicate drift.

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_pad::{button, AnalogSticks, PadMode, PadState};

const PORT_COUNT: usize = 2;
const DRIFT_SETTLE_FRAMES: u8 = 30;
const DRIFT_SAMPLE_FRAMES: u8 = 90;
const EXIT_HOLD_FRAMES: u8 = 45;
const GOOD_OFFSET: u8 = 8;
const WARN_OFFSET: u8 = 16;

const INK: (u8, u8, u8) = (226, 232, 240);
const MUTED: (u8, u8, u8) = (118, 138, 168);
const GOOD: (u8, u8, u8) = (92, 232, 132);
const WARN: (u8, u8, u8) = (255, 210, 80);
const BAD: (u8, u8, u8) = (255, 92, 92);
const HELD: (u8, u8, u8) = (255, 232, 128);

#[derive(Copy, Clone)]
struct DriftMonitor {
    last: AnalogSticks,
    has_last: bool,
    settle_frames: u8,
    sample_frames: u8,
    peak_offset: u8,
    ready: bool,
}

impl DriftMonitor {
    const fn new() -> Self {
        Self {
            last: AnalogSticks::CENTERED,
            has_last: false,
            settle_frames: 0,
            sample_frames: 0,
            peak_offset: 0,
            ready: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn update(&mut self, pad: PadState) {
        if !pad.mode.has_sticks() {
            self.reset();
            return;
        }

        let sticks = pad.sticks;
        if !self.has_last {
            self.last = sticks;
            self.has_last = true;
            self.settle_frames = 1;
            return;
        }

        // Motion resets the measurement automatically. This lets every axis be
        // exercised freely; once both sticks are released, a fresh centre test
        // begins without dedicating one of the controller buttons to RESET.
        if stick_motion(sticks, self.last) > 4 {
            self.settle_frames = 0;
            self.sample_frames = 0;
            self.peak_offset = 0;
            self.ready = false;
        } else if self.settle_frames < DRIFT_SETTLE_FRAMES {
            self.settle_frames += 1;
        } else if self.sample_frames < DRIFT_SAMPLE_FRAMES {
            self.peak_offset = self.peak_offset.max(stick_offset(sticks));
            self.sample_frames += 1;
            self.ready = self.sample_frames == DRIFT_SAMPLE_FRAMES;
        }

        self.last = sticks;
    }
}

/// Stateful, interactive controller screen. Button history is deliberately
/// per-connection: unplugging a controller clears its green "seen" markers so
/// a replacement pad cannot inherit the previous pad's result.
pub struct ControllerTest {
    pads: [PadState; PORT_COUNT],
    seen_buttons: [u16; PORT_COUNT],
    connected: [bool; PORT_COUNT],
    drift: [DriftMonitor; PORT_COUNT],
    exit_hold: u8,
}

impl ControllerTest {
    pub const fn new() -> Self {
        Self {
            pads: [PadState::NONE; PORT_COUNT],
            seen_buttons: [0; PORT_COUNT],
            connected: [false; PORT_COUNT],
            drift: [DriftMonitor::new(); PORT_COUNT],
            exit_hold: 0,
        }
    }

    pub fn start(&mut self) {
        *self = Self::new();

        // DualShock pads may boot in digital mode. Ask both ports for analog
        // mode up front so the drift test works without an obscure setup step;
        // digital-only pads safely ignore the request.
        let _ = psx_pad::enable_analog_port1();
        let _ = psx_pad::enable_analog_port2();
        self.pads = [psx_pad::poll_port1(), psx_pad::poll_port2()];
        self.connected = [self.pads[0].is_connected(), self.pads[1].is_connected()];
    }

    /// Refresh both ports. Returns `true` once START+SELECT has been held long
    /// enough on either pad to leave the screen. A hold gesture is used so both
    /// buttons remain independently testable instead of START being stolen by
    /// navigation on the first frame it is pressed.
    pub fn update(&mut self, port1: PadState) -> bool {
        let mut pads = [port1, psx_pad::poll_port2()];

        let mut port = 0usize;
        while port < PORT_COUNT {
            let is_connected = pads[port].is_connected();
            if is_connected && !self.connected[port] {
                if port == 0 {
                    let _ = psx_pad::enable_analog_port1();
                    pads[port] = psx_pad::poll_port1();
                } else {
                    let _ = psx_pad::enable_analog_port2();
                    pads[port] = psx_pad::poll_port2();
                }
                self.seen_buttons[port] = 0;
                self.drift[port].reset();
            } else if !is_connected && self.connected[port] {
                self.seen_buttons[port] = 0;
                self.drift[port].reset();
            }

            self.connected[port] = pads[port].is_connected();
            self.seen_buttons[port] |= pads[port].buttons.bits();
            self.drift[port].update(pads[port]);
            port += 1;
        }

        self.pads = pads;
        let exit_mask = button::START | button::SELECT;
        let exit_held = pads
            .iter()
            .any(|pad| pad.buttons.bits() & exit_mask == exit_mask);
        self.exit_hold = if exit_held {
            self.exit_hold.saturating_add(1)
        } else {
            0
        };
        self.exit_hold >= EXIT_HOLD_FRAMES
    }

    pub fn draw(&self, font: &FontAtlas) {
        gpu::draw_rect_flat(0, 0, 320, 240, 5, 8, 18);
        gpu::draw_rect_flat(0, 0, 320, 25, 12, 20, 40);
        gpu::draw_rect_flat(0, 204, 320, 36, 8, 14, 30);

        font.draw_text(8, 8, "CONTROLLER TEST", INK);
        let connected = match (self.pads[0].is_connected(), self.pads[1].is_connected()) {
            (false, false) => "NO CONTROLLERS",
            (true, false) => "PORT 1 ONLY",
            (false, true) => "PORT 2 ONLY",
            (true, true) => "BOTH PORTS",
        };
        let connected_x = 312 - font.text_width(connected) as i16;
        font.draw_text(
            connected_x,
            8,
            connected,
            if connected == "NO CONTROLLERS" {
                WARN
            } else {
                GOOD
            },
        );

        self.draw_port(font, 0, 4);
        self.draw_port(font, 1, 162);

        font.draw_text(8, 207, "HELD:YELLOW  SEEN:GREEN  NEW:GREY", MUTED);
        font.draw_text(8, 218, "DRIFT <=8 GOOD  9-16 WARN  >16 BAD", MUTED);
        font.draw_text(8, 229, "HOLD START+SELECT FOR MENU", MUTED);
        if self.exit_hold != 0 {
            font.draw_text(240, 229, dec3(self.exit_hold as u16).as_str(), HELD);
            font.draw_text(264, 229, "/045", MUTED);
        }
    }

    fn draw_port(&self, font: &FontAtlas, port: usize, x: i16) {
        let pad = self.pads[port];
        let outer = if pad.is_connected() {
            (42, 70, 104)
        } else {
            (40, 46, 62)
        };
        gpu::draw_rect_flat(x, 29, 154, 172, outer.0, outer.1, outer.2);
        gpu::draw_rect_flat(x + 1, 30, 152, 170, 8, 13, 27);

        font.draw_text(x + 6, 34, if port == 0 { "PORT 1" } else { "PORT 2" }, INK);
        let kind = pad_kind(pad.mode);
        let kind_color = if pad.is_connected() { GOOD } else { MUTED };
        let kind_x = x + 148 - font.text_width(kind) as i16;
        font.draw_text(kind_x, 34, kind, kind_color);

        let required = required_buttons(pad.mode);
        let seen = self.seen_buttons[port] & required;
        font.draw_text(x + 6, 46, "BUTTONS", MUTED);
        font.draw_text(x + 70, 46, dec3(seen.count_ones() as u16).as_str(), INK);
        font.draw_text(x + 94, 46, "/", MUTED);
        font.draw_text(
            x + 102,
            46,
            dec3(required.count_ones() as u16).as_str(),
            INK,
        );
        if required != 0 && seen == required {
            font.draw_text(x + 130, 46, "OK", GOOD);
        }

        let buttons = [
            ("UP", button::UP),
            ("DOWN", button::DOWN),
            ("LEFT", button::LEFT),
            ("RGHT", button::RIGHT),
            ("TRI", button::TRIANGLE),
            ("CIR", button::CIRCLE),
            ("X", button::CROSS),
            ("SQR", button::SQUARE),
            ("L1", button::L1),
            ("R1", button::R1),
            ("L2", button::L2),
            ("R2", button::R2),
            ("STRT", button::START),
            ("SEL", button::SELECT),
            ("L3", button::L3),
            ("R3", button::R3),
        ];
        let mut index = 0usize;
        while index < buttons.len() {
            let column = (index % 4) as i16;
            let row = (index / 4) as i16;
            draw_button_chip(
                font,
                x + 5 + column * 36,
                58 + row * 12,
                buttons[index].0,
                pad.buttons.bits() & buttons[index].1 != 0,
                self.seen_buttons[port] & buttons[index].1 != 0,
            );
            index += 1;
        }

        if pad.mode.has_sticks() {
            draw_stick(x + 12, 108, pad.sticks.left_x, pad.sticks.left_y);
            draw_stick(x + 94, 108, pad.sticks.right_x, pad.sticks.right_y);
            font.draw_text(x + 32, 110, "L", INK);
            font.draw_text(x + 114, 110, "R", INK);

            font.draw_text(x + 6, 158, "LX", MUTED);
            font.draw_text(x + 24, 158, dec3(pad.sticks.left_x as u16).as_str(), INK);
            font.draw_text(x + 56, 158, "LY", MUTED);
            font.draw_text(x + 74, 158, dec3(pad.sticks.left_y as u16).as_str(), INK);
            font.draw_text(x + 6, 169, "RX", MUTED);
            font.draw_text(x + 24, 169, dec3(pad.sticks.right_x as u16).as_str(), INK);
            font.draw_text(x + 56, 169, "RY", MUTED);
            font.draw_text(x + 74, 169, dec3(pad.sticks.right_y as u16).as_str(), INK);
            draw_drift_status(font, x + 6, 185, self.drift[port]);
        } else if pad.is_connected() {
            font.draw_text(x + 14, 126, "DIGITAL PAD", INK);
            font.draw_text(x + 14, 140, "STICKS / DRIFT N/A", MUTED);
        } else {
            font.draw_text(x + 14, 126, "CONNECT CONTROLLER", MUTED);
            font.draw_text(x + 14, 140, "PORT IS LIVE", MUTED);
        }
    }
}

fn required_buttons(mode: PadMode) -> u16 {
    if !mode.is_connected() {
        0
    } else if mode.has_sticks() {
        u16::MAX
    } else {
        u16::MAX & !(button::L3 | button::R3)
    }
}

const fn pad_kind(mode: PadMode) -> &'static str {
    match mode {
        PadMode::Disconnected => "EMPTY",
        PadMode::Digital => "DIGITAL",
        PadMode::Analog => "DUALSHOCK",
        PadMode::Config => "CONFIG",
        PadMode::Unknown => "UNKNOWN",
    }
}

fn draw_button_chip(font: &FontAtlas, x: i16, y: i16, label: &'static str, held: bool, seen: bool) {
    let (background, foreground) = if held {
        ((96, 78, 18), HELD)
    } else if seen {
        ((14, 70, 44), GOOD)
    } else {
        ((24, 30, 46), MUTED)
    };
    gpu::draw_rect_flat(x, y, 32, 10, background.0, background.1, background.2);
    let text_x = x + (32 - font.text_width(label) as i16) / 2;
    font.draw_text(text_x, y + 1, label, foreground);
}

fn draw_stick(x: i16, y: i16, raw_x: u8, raw_y: u8) {
    gpu::draw_rect_flat(x, y, 48, 48, 42, 58, 82);
    gpu::draw_rect_flat(x + 1, y + 1, 46, 46, 7, 12, 24);
    gpu::draw_rect_flat(x + 20, y + 20, 8, 8, 12, 54, 36);
    gpu::draw_line_mono(x + 24, y + 2, x + 24, y + 45, 56, 74, 100);
    gpu::draw_line_mono(x + 2, y + 24, x + 45, y + 24, 56, 74, 100);

    let px = x + 2 + (raw_x as i16 * 43) / 255;
    let py = y + 2 + (raw_y as i16 * 43) / 255;
    let offset = axis_offset(raw_x).max(axis_offset(raw_y));
    let color = drift_color(offset);
    gpu::draw_rect_flat(px - 2, py - 2, 5, 5, color.0, color.1, color.2);
}

fn draw_drift_status(font: &FontAtlas, x: i16, y: i16, drift: DriftMonitor) {
    if drift.ready {
        let (label, color) = if drift.peak_offset <= GOOD_OFFSET {
            ("CENTER PASS", GOOD)
        } else if drift.peak_offset <= WARN_OFFSET {
            ("SMALL DRIFT", WARN)
        } else {
            ("DRIFT FAIL", BAD)
        };
        font.draw_text(x, y, label, color);
        font.draw_text(x + 96, y, "MAX", MUTED);
        font.draw_text(x + 122, y, dec3(drift.peak_offset as u16).as_str(), color);
    } else if drift.settle_frames < DRIFT_SETTLE_FRAMES {
        font.draw_text(x, y, "RELEASE / HOLD STILL", WARN);
    } else {
        font.draw_text(x, y, "SAMPLING", MUTED);
        font.draw_text(x + 72, y, dec3(drift.sample_frames as u16).as_str(), INK);
        font.draw_text(x + 96, y, "/090", MUTED);
    }
}

const fn axis_offset(value: u8) -> u8 {
    if value >= 0x80 {
        value - 0x80
    } else {
        0x80 - value
    }
}

fn stick_offset(sticks: AnalogSticks) -> u8 {
    axis_offset(sticks.left_x)
        .max(axis_offset(sticks.left_y))
        .max(axis_offset(sticks.right_x))
        .max(axis_offset(sticks.right_y))
}

fn stick_motion(now: AnalogSticks, before: AnalogSticks) -> u8 {
    now.left_x
        .abs_diff(before.left_x)
        .max(now.left_y.abs_diff(before.left_y))
        .max(now.right_x.abs_diff(before.right_x))
        .max(now.right_y.abs_diff(before.right_y))
}

const fn drift_color(offset: u8) -> (u8, u8, u8) {
    if offset <= GOOD_OFFSET {
        GOOD
    } else if offset <= WARN_OFFSET {
        WARN
    } else {
        BAD
    }
}

struct Dec3 {
    bytes: [u8; 3],
}

impl Dec3 {
    fn as_str(&self) -> &str {
        // Every byte is built from an ASCII decimal digit.
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
    }
}

fn dec3(value: u16) -> Dec3 {
    let value = value.min(999);
    Dec3 {
        bytes: [
            b'0' + ((value / 100) % 10) as u8,
            b'0' + ((value / 10) % 10) as u8,
            b'0' + (value % 10) as u8,
        ],
    }
}
