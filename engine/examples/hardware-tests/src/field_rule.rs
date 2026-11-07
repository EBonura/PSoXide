// SPDX-License-Identifier: GPL-2.0-or-later
//! The 480i draw rule, v2.4 (records `0x800`-`0x852`).
//!
//! In a 480-line interlaced mode the GPU does not draw to the rows of the
//! field being displayed when GP0 E1h bit 10 ("drawing to display area") is
//! clear. The emulator assumes that rows whose `y & 1` equals GPUSTAT bit 31
//! are skipped by fills, polygons, rectangles and lines, and are not skipped
//! by VRAM-to-VRAM copies and CPU-to-VRAM uploads. Valkyrie Profile is the
//! only thing that pins it. This asks the console.
//!
//! One case is one command that covers 16 rows of a 64 x 16 scratch area,
//! issued shortly after a VBlank (so the field cannot flip under it) while
//! GPUSTAT bit 31 has the wanted value. The scratch area is cleared by an
//! upload first and read back afterwards; the record is a 16-bit mask of the
//! rows that now hold something.
//!
//! Every record describes itself (`min` the row mask, `med` the descriptor
//! below, `max` the flags below), so a record cannot be mistaken for another
//! case whatever order the table is in.
//!

use crate::console_tests::record;
use crate::v24::{self, xy};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_io::gpu as gpu_io;
use psx_io::timers::{self, Timer};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record id of the table. The table is 83 records.
/// rec field_rule: row_mask, descriptor, flags (the 480i draw rule; descriptor bits 0 to 3 the command, 0 fill, 1 rectangle, 2 two triangles, 3 textured quad, 4 line, 5 VRAM copy, 6 upload, 7 textured quad whose texpage word sets bit 10; bit 4 E1 bit 10; bits 5 and 6 the field asked for, 0 or 1, both set for any; bits 7 and 8 the display mode, 0 480 interlaced, 1 480 not interlaced, 2 240 interlaced, 3 480 interlaced 24bpp; bit 9 the scratch area outside the displayed rectangle; bits 10 to 12 the variant, 0 plain, 1 draw area top on an odd row, 2 draw offset odd, 3 display start Y odd, 4 issued at once after VBlank, 5 timed fill 256 x 240, 6 timed rectangle 256 x 240, 7 display blanked (GP1 03h) while the command is issued; flags bit 0 GPUSTAT bit 31 at issue, bit 1 after, bits 2 to 4 attempts to get the wanted field, bit 5 the scratch area was not clear before, bit 6 the command did not drain, bit 7 the field never matched, bit 8 GPUSTAT bit 19 at issue, bit 9 bit 22, bit 10 bit 21; the timed variants hold the clocks divided by 8 in the first field instead of the mask)
pub(crate) const FIELD_RULE_RECORD: u16 = 0x800;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Cmd {
    Fill = 0,
    Rect = 1,
    Tris = 2,
    TexQuad = 3,
    Line = 4,
    Copy = 5,
    Upload = 6,
    TexQuadTpage10 = 7,
}

const PLAIN_CMDS: [Cmd; 7] = [
    Cmd::Fill,
    Cmd::Rect,
    Cmd::Tris,
    Cmd::TexQuad,
    Cmd::Line,
    Cmd::Copy,
    Cmd::Upload,
];

/// GP1(08h) values: 512 wide, 15bpp NTSC, and the vertical bits under test.
const MODE_480I: u32 = 0x26;
const MODE_480_PROGRESSIVE: u32 = 0x06;
const MODE_240_INTERLACED: u32 = 0x22;
const MODE_480I_24BPP: u32 = 0x36;

const SCRATCH_W: u32 = 64;
const SCRATCH_H: u32 = 16;
const SCRATCH_WORDS: u32 = SCRATCH_W * SCRATCH_H / 2;
/// Scratch inside the displayed rectangle (512 x 480 from the origin) and
/// outside it.
const INSIDE: (u32, u32) = (0, 256);
const OUTSIDE: (u32, u32) = (640, 256);
/// A 64 x 16 block of white 15bpp: the texture (a texpage on x = 576) and the
/// VRAM copy's source.
const TEXTURE: (u32, u32) = (576, 0);
const COPY_SOURCE: (u32, u32) = (576, 32);
const TEXPAGE_15BPP: u32 = 9 | (2 << 7);
/// Cycles after the VBlank edge to wait before sampling the field: well into
/// the active picture (a 480i field's VBlank is a little over a millisecond).
const SETTLE_CYCLES: u32 = 100_000;
const ATTEMPTS: u32 = 6;
/// The timed fills and rectangles cover this much.
const COST_W: u32 = 256;
const COST_H: u32 = 240;

#[derive(Copy, Clone)]
struct Case {
    cmd: Cmd,
    bit10: bool,
    /// 0, 1 or 2 for any.
    field: u32,
    mode: u32,
    outside: bool,
    variant: u32,
}

const fn plain(cmd: Cmd, bit10: bool, field: u32) -> Case {
    Case {
        cmd,
        bit10,
        field,
        mode: 0,
        outside: false,
        variant: 0,
    }
}

impl Case {
    fn descriptor(&self) -> u32 {
        let field_bits = match self.field {
            0 => 0,
            1 => 1,
            _ => 3,
        };
        (self.cmd as u32)
            | ((self.bit10 as u32) << 4)
            | (field_bits << 5)
            | (self.mode << 7)
            | ((self.outside as u32) << 9)
            | (self.variant << 10)
    }
}

fn mode_register(mode: u32) -> u32 {
    match mode {
        0 => MODE_480I,
        1 => MODE_480_PROGRESSIVE,
        2 => MODE_240_INTERLACED,
        _ => MODE_480I_24BPP,
    }
}

/// Put `mode` on the display, starting at row `start_y`, and let it settle.
fn show_mode(mode: u32, start_y: u32) {
    gpu_io::write_display_control(0x0300_0000); // display on
    gpu_io::write_display_control(0x0800_0000 | mode_register(mode));
    gpu_io::write_display_control(0x0500_0000 | (start_y << 10));
    for _ in 0..3 {
        v24::frame();
    }
}

fn restore_display() {
    probe_gpu!(gpu);
    gpu.set_display(DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240));
    gpu.set_display_start((0, 0));
    gpu.set_display_enabled(true);
    v24::environment(0);
    for _ in 0..2 {
        v24::frame();
    }
}

/// The white blocks the textured quad samples and the copy reads.
fn prepare_sources() {
    v24::upload(
        TEXTURE.0,
        TEXTURE.1,
        SCRATCH_W,
        SCRATCH_H,
        0x7FFF_7FFF,
        SCRATCH_WORDS,
    );
    v24::upload(
        COPY_SOURCE.0,
        COPY_SOURCE.1,
        SCRATCH_W,
        SCRATCH_H,
        0x7FFF_7FFF,
        SCRATCH_WORDS,
    );
    v24::drain();
}

/// Mask of the scratch rows that hold a non-zero pixel. `None` when the read
/// never answered.
fn row_mask(at: (u32, u32)) -> Option<u32> {
    let mut mask = 0u32;
    let mut index = 0u32;
    let words_per_row = SCRATCH_W / 2;
    let ok = v24::read_rect(at.0, at.1, SCRATCH_W, SCRATCH_H, |word| {
        if word != 0 {
            mask |= 1 << (index / words_per_row);
        }
        index += 1;
    });
    if ok {
        Some(mask)
    } else {
        None
    }
}

fn clear_scratch(at: (u32, u32)) -> bool {
    v24::upload(at.0, at.1, SCRATCH_W, SCRATCH_H, 0, SCRATCH_WORDS);
    v24::drain() && row_mask(at) == Some(0)
}

/// Issue the case's command over the scratch area at `at`.
fn issue(case: &Case, at: (u32, u32)) {
    let (x, y) = at;
    match case.cmd {
        Cmd::Fill => v24::send(&[0x02FF_FFFF, xy(x, y), xy(SCRATCH_W, SCRATCH_H)]),
        Cmd::Rect => match case.variant {
            // Relative y of -1 under an odd draw offset of 257: final rows
            // start at 256.
            2 => v24::send(&[
                0x60FF_FFFF,
                xy(x, 0xFFFF & (y.wrapping_sub(257))),
                xy(SCRATCH_W, SCRATCH_H),
            ]),
            _ => v24::send(&[0x60FF_FFFF, xy(x, y), xy(SCRATCH_W, SCRATCH_H)]),
        },
        Cmd::Tris => {
            v24::send(&[
                0x20FF_FFFF,
                xy(x, y),
                xy(x + SCRATCH_W, y),
                xy(x, y + SCRATCH_H),
            ]);
            v24::send(&[
                0x20FF_FFFF,
                xy(x + SCRATCH_W, y + SCRATCH_H),
                xy(x, y + SCRATCH_H),
                xy(x + SCRATCH_W, y),
            ]);
        }
        Cmd::TexQuad | Cmd::TexQuadTpage10 => {
            let page = if case.cmd == Cmd::TexQuadTpage10 {
                TEXPAGE_15BPP | (1 << 10)
            } else {
                TEXPAGE_15BPP
            };
            v24::send(&[
                0x2D80_8080,
                xy(x, y),
                0,
                xy(x + SCRATCH_W, y),
                (page << 16) | 63,
                xy(x, y + SCRATCH_H),
                15 << 8,
                xy(x + SCRATCH_W, y + SCRATCH_H),
                (15 << 8) | 63,
            ]);
        }
        Cmd::Line => v24::send(&[0x40FF_FFFF, xy(x + 8, y), xy(x + 8, y + SCRATCH_H - 1)]),
        Cmd::Copy => v24::send(&[
            0x8000_0000,
            xy(COPY_SOURCE.0, COPY_SOURCE.1),
            xy(x, y),
            xy(SCRATCH_W, SCRATCH_H),
        ]),
        Cmd::Upload => {
            v24::send(&[0xA000_0000, xy(x, y), xy(SCRATCH_W, SCRATCH_H)]);
            for _ in 0..SCRATCH_WORDS {
                gpu_io::write_command(0x7FFF_7FFF);
            }
        }
    }
}

/// Draw environment of the case: E1 with bit 10 as asked, and for the
/// variants an odd draw-area top or an odd draw offset.
fn set_environment(case: &Case, at: (u32, u32)) {
    v24::environment((case.bit10 as u32) << 10);
    match case.variant {
        1 => {
            // Draw area top on an odd row of the scratch area.
            gpu_io::write_command(0xE300_0000 | (at.1 + 1) << 10 | at.0);
        }
        2 => {
            gpu_io::write_command(0xE500_0000 | (257 << 11));
        }
        _ => {}
    }
}

/// One case. Returns the record.
fn run_case(id: u16, case: &Case) -> TimingRecord {
    crate::bounds::record_start(id);
    let at = if case.outside { OUTSIDE } else { INSIDE };
    let start_y = if case.variant == 3 { 1 } else { 0 };
    show_mode(case.mode, start_y);
    set_environment(case, at);
    let clean = clear_scratch(at);
    if case.variant == 7 {
        // Display blanked: the rule may be off then.
        gpu_io::write_display_control(0x0300_0001);
        for _ in 0..2 {
            v24::frame();
        }
    }

    let mut attempts = 0u32;
    let mut matched = false;
    let mut s1 = 0u32;
    while attempts < ATTEMPTS {
        attempts += 1;
        v24::frame();
        if case.variant != 4 {
            v24::spin_cycles(SETTLE_CYCLES);
        }
        s1 = v24::field();
        if case.field > 1 || s1 == case.field {
            matched = true;
            break;
        }
    }
    // Issued whether or not the field came up: the flag says it did not.
    let stat = gpu_io::status().bits();
    issue(case, at);
    let drained = v24::drain();
    let s2 = v24::field();
    let mask = row_mask(at).unwrap_or(0xFFFF);
    let flags = s1
        | (s2 << 1)
        | ((attempts - 1).min(7) << 2)
        | ((!clean as u32) << 5)
        | ((!drained as u32) << 6)
        | ((!matched as u32) << 7)
        | (((stat >> 19) & 1) << 8)
        | (((stat >> 22) & 1) << 9)
        | (((stat >> 21) & 1) << 10)
        | (((stat >> 23) & 1) << 11);
    record(id, mask, case.descriptor(), flags)
}

/// A timed fill or rectangle over 256 x 240 of the outside area, clocks
/// divided by 8, from the first word sent to the drain marker.
fn run_cost(id: u16, case: &Case) -> TimingRecord {
    crate::bounds::record_start(id);
    let at = OUTSIDE;
    show_mode(0, 0);
    v24::environment((case.bit10 as u32) << 10);
    let mut attempts = 0u32;
    let mut matched = false;
    let mut s1 = 0u32;
    while attempts < ATTEMPTS {
        attempts += 1;
        v24::frame();
        v24::spin_cycles(SETTLE_CYCLES);
        s1 = v24::field();
        if s1 == case.field {
            matched = true;
            break;
        }
    }
    timers::set_mode(Timer::Timer2, 0x0200); // system clock / 8
    timers::set_counter(Timer::Timer2, 0);
    match case.cmd {
        Cmd::Fill => v24::send(&[0x02FF_FFFF, xy(at.0, at.1), xy(COST_W, COST_H)]),
        _ => v24::send(&[0x60FF_FFFF, xy(at.0, at.1), xy(COST_W, COST_H)]),
    }
    let drained = v24::drain();
    let ticks = timers::counter(Timer::Timer2) as u32;
    let s2 = v24::field();
    let flags = s1
        | (s2 << 1)
        | ((attempts - 1).min(7) << 2)
        | ((!drained as u32) << 6)
        | ((!matched as u32) << 7);
    record(id, ticks, case.descriptor(), flags)
}

/// The whole table.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    let mut id = FIELD_RULE_RECORD;
    let push = |record: TimingRecord, records: &mut Records, next: &mut usize| {
        push_timing_record(records, next, record);
    };
    prepare_sources();

    // The rule itself: every command, bit 10 clear and set, both fields.
    for bit10 in [false, true] {
        for field in [0u32, 1] {
            for cmd in PLAIN_CMDS {
                let r = run_case(id, &plain(cmd, bit10, field));
                push(r, records, next);
                id += 1;
            }
        }
    }
    // Is the rule about the displayed rectangle or all of VRAM?
    for field in [0u32, 1] {
        for cmd in [Cmd::Fill, Cmd::Rect, Cmd::Tris] {
            let mut case = plain(cmd, false, field);
            case.outside = true;
            let r = run_case(id, &case);
            push(r, records, next);
            id += 1;
        }
    }
    // The other modes: the rule should not apply.
    for mode in 1..4u32 {
        for cmd in PLAIN_CMDS {
            let mut case = plain(cmd, false, 2);
            case.mode = mode;
            let r = run_case(id, &case);
            push(r, records, next);
            id += 1;
        }
    }
    // A polygon's own texpage word with bit 10 set while E1 bit 10 is clear.
    for field in [0u32, 1] {
        let r = run_case(id, &plain(Cmd::TexQuadTpage10, false, field));
        push(r, records, next);
        id += 1;
    }
    // Draw area top on an odd row, and an odd draw offset: is the skipped
    // parity the absolute VRAM row?
    for variant in [1u32, 2] {
        for field in [0u32, 1] {
            let mut case = plain(Cmd::Rect, false, field);
            case.variant = variant;
            let r = run_case(id, &case);
            push(r, records, next);
            id += 1;
        }
    }
    // Display start on an odd row.
    for field in [0u32, 1] {
        let mut case = plain(Cmd::Fill, false, field);
        case.variant = 3;
        let r = run_case(id, &case);
        push(r, records, next);
        id += 1;
    }
    // A fill issued at once after the VBlank edge, on four frames in a row.
    for _ in 0..4 {
        let mut case = plain(Cmd::Fill, false, 2);
        case.variant = 4;
        let r = run_case(id, &case);
        push(r, records, next);
        id += 1;
    }
    // Does the GPU charge for rows it skips?
    for (cmd, variant) in [(Cmd::Fill, 5u32), (Cmd::Rect, 6)] {
        for bit10 in [false, true] {
            for field in [0u32, 1] {
                let mut case = plain(cmd, bit10, field);
                case.variant = variant;
                let r = run_cost(id, &case);
                push(r, records, next);
                id += 1;
            }
        }
    }
    // The rule with the display blanked: is it off then?
    for field in [0u32, 1] {
        for cmd in [Cmd::Fill, Cmd::Rect, Cmd::Tris, Cmd::Line] {
            let mut case = plain(cmd, false, field);
            case.variant = 7;
            let r = run_case(id, &case);
            push(r, records, next);
            id += 1;
        }
    }
    restore_display();
}
