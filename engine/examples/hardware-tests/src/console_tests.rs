// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared pieces of the cases that take over the display and the drive for a
//! while: the kernel timing, the display modes, the XA loop and the CD stream
//! cases. They run as steps of the linear run (`run.rs`), draw their own
//! progress, never wait for a person, and leave timing-block records in the
//! `0x2C0`-`0x315` range. The record layout is in each module and in
//! tools/hwtest-report.py.

use crate::report::hex2;
use crate::TimingRecord;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_rt::{interrupts, tty};

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
/// CD STREAM COST (v1.28): `0x2F0` to `0x2F7`.
pub(crate) const CDCOST_RECORD: u16 = 0x2F0;
pub(crate) const CDCOST_COUNT: usize = 8;
/// CD-DA HANDOFF (v1.28): `0x300` to `0x30B`.
pub(crate) const CDHANDOFF_RECORD: u16 = 0x300;
pub(crate) const CDHANDOFF_COUNT: usize = 12;
/// CD MOTOR (v1.28): `0x310` to `0x315`.
pub(crate) const CDMOTOR_RECORD: u16 = 0x310;
pub(crate) const CDMOTOR_COUNT: usize = 6;
/// One timing record whose three fields carry values of the case's own.
pub(crate) fn record(id: u16, a: u32, b: u32, c: u32) -> TimingRecord {
    let clamp = |value: u32| {
        // u32::MAX is a deliberate "none" and reads 0xFFFF either way.
        if value > 0xFFFF && value != u32::MAX {
            // SAFETY: single thread; plain statics.
            unsafe {
                crate::CLAMPED_FIELDS += 1;
                if crate::FIRST_CLAMPED_ID == 0 {
                    crate::FIRST_CLAMPED_ID = id;
                }
                crate::LAST_CLAMPED_ID = id;
            }
            tty::print("hardware-tests: field over 16 bits clamped in rec ");
            tty::print(hex2((id >> 8) as u8).as_str());
            tty::print(hex2(id as u8).as_str());
            tty::print("\n");
        }
        value.min(0xFFFF) as u16
    };
    TimingRecord {
        id,
        work: 0,
        min: clamp(a),
        med: clamp(b),
        max: clamp(c),
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
