// SPDX-License-Identifier: GPL-2.0-or-later
//! v1.27 CONSOLE TESTS (MAIN MENU row 9): four cases that settle open
//! questions in one console session.
//!
//! - `kernel_timing`: the real BIOS's cost of EnterCriticalSection,
//!   ExitCriticalSection and a VBlank interrupt round trip, to calibrate the
//!   emulator's kernel timing.
//! - `display_widths`: a test pattern with edge markers at 256, 320, 368, 384,
//!   512 and 640 pixels wide, and a 640x480 interlaced pattern.
//! - `xa_loop`: a short generated XA song on loop, with the restart gap and
//!   whether GetlocP keeps updating while it streams.
//!
//! Each case runs as a blocking screen of its own (like the FMV test), keeps
//! its result on screen until CROSS, and leaves timing-block records `0x2C0`
//! and up in the capture, so the QR pages that open afterwards carry it. The
//! record layout is in each module's `records` and in tools/hwtest-report.py.

use crate::{TimingRecord, TIMING_RECORD_COUNT, TIMING_RECORD_UNUSED};
use psx_engine::button;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_pad::ButtonState;
use psx_rt::interrupts;

/// Kernel timing: `0x2C0` to `0x2C5`.
pub(crate) const KERNEL_RECORD: u16 = 0x2C0;
pub(crate) const KERNEL_COUNT: usize = 6;
/// Display widths, one record per width in screen order: `0x2D0` to `0x2D5`.
pub(crate) const WIDTH_RECORD: u16 = 0x2D0;
pub(crate) const WIDTH_COUNT: usize = 6;
/// 480-line interlace: `0x2D6` and `0x2D7`.
pub(crate) const INTERLACE_RECORD: u16 = 0x2D6;
pub(crate) const INTERLACE_COUNT: usize = 2;
/// XA music loop: `0x2E0` to `0x2E3`.
pub(crate) const XA_RECORD: u16 = 0x2E0;
pub(crate) const XA_COUNT: usize = 4;
/// Slots the four cases can fill in the timing report.
pub(crate) const RECORD_SLOTS: usize = KERNEL_COUNT + WIDTH_COUNT + INTERLACE_COUNT + XA_COUNT;

/// What the last run of each case left, folded into every later capture that
/// carries the timing block.
#[derive(Copy, Clone)]
pub(crate) struct Results {
    pub(crate) kernel: Option<[TimingRecord; KERNEL_COUNT]>,
    pub(crate) widths: Option<[TimingRecord; WIDTH_COUNT]>,
    pub(crate) interlace: Option<[TimingRecord; INTERLACE_COUNT]>,
    pub(crate) xa: Option<[TimingRecord; XA_COUNT]>,
}

impl Results {
    pub(crate) const fn new() -> Self {
        Self {
            kernel: None,
            widths: None,
            interlace: None,
            xa: None,
        }
    }

    /// Every record the cases have left so far.
    pub(crate) fn for_each(&self, mut f: impl FnMut(&TimingRecord)) {
        let groups: [&[TimingRecord]; 4] = [
            self.kernel.as_ref().map_or(&[], |r| r.as_slice()),
            self.widths.as_ref().map_or(&[], |r| r.as_slice()),
            self.interlace.as_ref().map_or(&[], |r| r.as_slice()),
            self.xa.as_ref().map_or(&[], |r| r.as_slice()),
        ];
        for record in groups.iter().flat_map(|group| group.iter()) {
            f(record);
        }
    }

    /// Put the records into `slots`, replacing an earlier run's in place or
    /// taking the first unused slot. False if the slots were full.
    pub(crate) fn merge(&self, slots: &mut [TimingRecord; TIMING_RECORD_COUNT]) -> bool {
        let mut all = true;
        self.for_each(|record| {
            let slot = slots
                .iter()
                .position(|slot| slot.id == record.id)
                .or_else(|| slots.iter().position(|s| s.id == TIMING_RECORD_UNUSED));
            match slot {
                Some(index) => slots[index] = *record,
                None => all = false,
            }
        });
        all
    }
}

/// One timing record whose three fields carry values of the case's own.
pub(crate) fn record(id: u16, a: u32, b: u32, c: u32) -> TimingRecord {
    let clamp = |value: u32| value.min(0xFFFF) as u16;
    TimingRecord {
        id,
        work: 0,
        min: clamp(a),
        med: clamp(b),
        max: clamp(c),
    }
}

/// Edge detection over the port-1 pad for a screen that runs its own loop.
pub(crate) struct Buttons {
    previous: ButtonState,
}

impl Buttons {
    /// Starts with whatever is held now counted as already pressed, so the
    /// CROSS that opened a screen cannot also act inside it.
    pub(crate) fn new() -> Self {
        Self {
            previous: psx_pad::poll_port1().buttons,
        }
    }

    /// Poll once; the mask of buttons that went down since the last call.
    pub(crate) fn poll(&mut self) -> Pressed {
        let now = psx_pad::poll_port1().buttons;
        let pressed = Pressed {
            now,
            before: self.previous,
        };
        self.previous = now;
        pressed
    }
}

pub(crate) struct Pressed {
    now: ButtonState,
    before: ButtonState,
}

impl Pressed {
    pub(crate) fn has(&self, mask: u16) -> bool {
        self.now.pressed_since(self.before, mask)
    }

    /// CROSS, START or TRIANGLE: the ways out of a case's screen.
    pub(crate) fn exit(&self) -> bool {
        self.has(button::CROSS) || self.has(button::START) || self.has(button::TRIANGLE)
    }
}

/// Decimal digits of `value`, as text.
pub(crate) fn number(buf: &mut [u8; 10], mut value: u32) -> &str {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[at..]).unwrap_or("?")
}

/// Hex digits of `value` (no prefix), eight of them.
pub(crate) fn hex(buf: &mut [u8; 8], value: u32) -> &str {
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = b"0123456789ABCDEF"[((value >> (28 - 4 * i)) & 0xF) as usize];
    }
    core::str::from_utf8(&buf[..]).unwrap_or("?")
}

/// Smallest, middle and largest of `values` (sorted in place). All zero when
/// there are none.
#[inline(never)]
pub(crate) fn spread(values: &mut [u32]) -> (u32, u32, u32) {
    if values.is_empty() {
        return (0, 0, 0);
    }
    // Insertion sort: the slices are a few dozen long, and the library sort
    // inlined at every caller cost kilobytes of boot EXE.
    for i in 1..values.len() {
        let mut j = i;
        while j > 0 && values[j - 1] > values[j] {
            values.swap(j - 1, j);
            j -= 1;
        }
    }
    (
        values[0],
        values[values.len() / 2],
        values[values.len() - 1],
    )
}

/// Wait for the next VBlank edge.
pub(crate) fn next_frame() {
    interrupts::wait_vblank();
}

/// A 320x240 double-buffered screen for a case that draws its own frames.
pub(crate) struct Screen<'a> {
    gpu: &'a mut Gpu,
    fb: DoubleBuffer,
}

impl<'a> Screen<'a> {
    /// Program the standard 320x240 display and take over the draw side.
    pub(crate) fn new(gpu: &'a mut Gpu) -> Self {
        gpu.set_display(DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240));
        let fb = DoubleBuffer::with_stride(Resolution::R320X240, 256);
        fb.apply_draw_target(gpu);
        gpu.set_display_start(fb.display_origin());
        Self { gpu, fb }
    }

    pub(crate) fn clear(&mut self, rgb: (u8, u8, u8)) {
        self.fb.clear(self.gpu, rgb);
    }

    /// Show the finished frame at once, without waiting for the VBlank edge:
    /// the GPU latches the new start at the next frame. For a loop that must
    /// keep polling something else.
    pub(crate) fn flip(&mut self) {
        self.gpu.wait_idle();
        self.fb.swap(self.gpu);
    }

    /// Finish the frame, wait for the VBlank edge and show it.
    pub(crate) fn present(&mut self) {
        self.gpu.wait_idle();
        next_frame();
        self.fb.swap(self.gpu);
    }
}

/// `value` as text at (x, y).
pub(crate) fn put_number(font: &FontAtlas, x: i16, y: i16, value: u32, colour: (u8, u8, u8)) {
    let mut digits = [0u8; 10];
    text(font, x, y, number(&mut digits, value), colour);
}

/// `console <name>=<value>` on the debug TTY, for the headless log.
pub(crate) fn tty_kv(case: &str, name: &str, value: u32) {
    let mut digits = [0u8; 10];
    psx_rt::tty::print("hardware-tests: console ");
    psx_rt::tty::print(case);
    psx_rt::tty::print(" ");
    psx_rt::tty::print(name);
    psx_rt::tty::print("=");
    psx_rt::tty::println(number(&mut digits, value));
}

/// The cases of the CONSOLE TESTS menu.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum ConsoleCase {
    KernelTiming,
    DisplayWidths,
    Interlace,
    XaLoop,
}

/// Run `case` on `gpu`, store what it measured in `results` and print the
/// headline numbers on the TTY. Blocks until the operator leaves the case.
pub(crate) fn run_case(gpu: &mut Gpu, results: &mut Results, case: ConsoleCase) {
    // The 320x240 cases keep the suite's own font where it always is.
    let font = || FontAtlas::upload(&psx_font::fonts::BASIC, crate::FONT_TPAGE, crate::FONT_CLUT);
    match case {
        ConsoleCase::KernelTiming => {
            let font = font();
            let out = crate::kernel_timing::run(&mut Screen::new(gpu), &font);
            results.kernel = Some(crate::kernel_timing::records(&out));
            tty_kv("kernel", "enter_med", out.enter.med);
            tty_kv("kernel", "exit_med", out.exit.med);
            tty_kv("kernel", "harness_med", out.harness.med);
            tty_kv("kernel", "bios_vblank_med", out.bios_vblank.med);
            tty_kv("kernel", "sdk_vblank_med", out.sdk_vblank.med);
        }
        ConsoleCase::DisplayWidths => {
            results.widths = Some(crate::display_widths::run_widths(gpu));
        }
        ConsoleCase::Interlace => {
            results.interlace = Some(crate::display_widths::run_interlace(gpu));
        }
        ConsoleCase::XaLoop => {
            let font = font();
            let run = crate::xa_loop::run(&mut Screen::new(gpu), &font);
            results.xa = Some(crate::xa_loop::records(&run));
            tty_kv("xa", "loops", run.loops);
            tty_kv("xa", "getlocp_updates", run.getlocp_updates() as u32);
        }
    }
}

/// Draw text at (x, y). Kept out of line: inlined at every call site the
/// glyph loop costs the boot EXE kilobytes (it has 502 sectors to fit in).
#[inline(never)]
pub(crate) fn text(font: &FontAtlas, x: i16, y: i16, s: &str, colour: (u8, u8, u8)) {
    font.draw_text(x, y, s, colour);
}

/// `flag` as bit `shift` of a flags word.
pub(crate) fn bit(flag: bool, shift: u32) -> u32 {
    (flag as u32) << shift
}
