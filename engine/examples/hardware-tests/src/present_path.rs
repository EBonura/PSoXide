// SPDX-License-Identifier: GPL-2.0-or-later
//! PRESENT ONE PATH: `psx_rt::present` on the console's own GPU, DMA and VBlank timing.
//!
//! [`FRAMES`] frames go through the real presenter and psx-rt's VBlank handler with the cap lifted (a
//! flip every VBlank when a frame is ready): frames of one chain, frames of two (`Part::More` then
//! `Part::Final`), and every fifth frame drawn through the GP0 port and handed over with
//! `present_drawn`. The CPU load varies from frame to frame, so some frames are kicked by the thread at
//! submit and some by the handler at an edge. Each frame paints a marker in a corner of its buffer
//! with a colour of its own. A VBlank hook records, at every edge, the flips and kicks so far and the
//! Timer 1 line (HBlanks since the VBlank reset) of the pass in which a flip was applied, and counts a
//! tear when a frame started drawing before the one before it was on screen, or a walk was running
//! while the display showed the buffer it draws into.
//!
//! Records, three fields each:
//!
//! * `0x970` frames flipped, parts kicked, and `tears << 8 | recoveries << 4 | bad markers`.
//! * `0x971` the earliest and latest Timer 1 line of a flip pass, and the most lines seen in a frame.
//! * `0x972` parts the thread kicked, parts the handler kicked, edges on which a part waited.
//! * `0x973` frames that waited past their edge, the most VBlanks from submit to flip, and the
//!   draw-done fallbacks the handler made.
//!
//! What a pass and a fail mean is in `docs/hardware-test-versions.md` (v2.17).

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::console_tests::record;
use crate::{gpu_read_word_at, push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_gpu::display::{DoubleBuffer, Resolution};
use psx_gpu::prim::FillRect;
use psx_io::timers::{self, Timer};
use psx_rt::present::{self, Part, PresentCap, Presenter};
use psx_rt::{dispatch, interrupts, tty};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record of the step: four records, `0x970` to `0x973`.
const RECORD: u16 = 0x970;
/// Frames presented.
const FRAMES: usize = 120;
/// Timer 1: HBlank clock, reset to zero at VBlank (sync mode 1).
const TIMER1_LINES_SINCE_VBLANK: u16 = 0x0103;
/// Where each frame paints its marker, 16 x 16 pixels.
const MARKER_X: u16 = 304;
/// Words of one side's arena: a triangle node, a marker node.
const ARENA_WORDS: usize = 16;

static mut ARENAS: [[u32; ARENA_WORDS]; 2] = [[0; ARENA_WORDS]; 2];

// The hook's state. It only reads and writes these while `RUNNING` is set; the
// thread writes `FIRST_PART` before each frame's first submit and reads the rest
// once `RUNNING` is clear.
static mut RUNNING: bool = false;
static mut KICK_BASE: u32 = 0;
static mut FLIP_BASE: u32 = 0;
static mut LAST_FLIPS: u32 = 0;
static mut STARTED: u32 = 0;
static mut TEARS: u32 = 0;
static mut LINE_MIN: u32 = u32::MAX;
static mut LINE_MAX: u32 = 0;
static mut LINES_PER_FRAME: u32 = 0;
static mut FIRST_PART: [u32; FRAMES + 1] = [u32::MAX; FRAMES + 1];
static mut FLIP_TICK: [u32; FRAMES] = [0; FRAMES];
static mut HOOKED: bool = false;

fn flips_total() -> u32 {
    interrupts::flip_histogram().iter().sum()
}

/// A 24-bit fill colour whose 15-bit form is exact, different per frame.
const fn marker_colour(frame: u32) -> (u8, u8, u8) {
    (
        ((frame * 40) & 0xF8) as u8,
        ((frame * 24 + 64) & 0xF8) as u8,
        0x80,
    )
}

const fn colour_15(c: (u8, u8, u8)) -> u32 {
    (c.0 as u32 >> 3) | ((c.1 as u32 >> 3) << 5) | ((c.2 as u32 >> 3) << 10)
}

/// Once per VBlank, after psx-rt's flip and kick.
fn hook() {
    // SAFETY: plain statics of this module, accessed through raw pointers; see their comment.
    unsafe {
        if !read_volatile(addr_of!(RUNNING)) {
            return;
        }
        let line = u32::from(timers::counter(Timer::Timer1));
        let flips = flips_total().wrapping_sub(read_volatile(addr_of!(FLIP_BASE)));
        if flips != read_volatile(addr_of!(LAST_FLIPS)) {
            write_volatile(addr_of_mut!(LAST_FLIPS), flips);
            let min = read_volatile(addr_of!(LINE_MIN)).min(line);
            let max = read_volatile(addr_of!(LINE_MAX)).max(line);
            write_volatile(addr_of_mut!(LINE_MIN), min);
            write_volatile(addr_of_mut!(LINE_MAX), max);
            if (flips as usize) <= FRAMES && flips > 0 {
                write_volatile(
                    addr_of_mut!(FLIP_TICK[flips as usize - 1]),
                    interrupts::vblank_count(),
                );
            }
        }
        let kicks = present::stats()
            .kick_count
            .wrapping_sub(read_volatile(addr_of!(KICK_BASE)));
        let mut started = read_volatile(addr_of!(STARTED));
        while (started as usize) < FRAMES
            && read_volatile(addr_of!(FIRST_PART[started as usize])) < kicks
        {
            started += 1;
        }
        write_volatile(addr_of_mut!(STARTED), started);
        let walking = psx_io::dma::is_busy(psx_io::dma::Channel::Gpu);
        if started > flips + 1 || (walking && started != flips + 1) {
            write_volatile(addr_of_mut!(TEARS), read_volatile(addr_of!(TEARS)) + 1);
        }
    }
}

/// Build frame `f`'s chain in `side`'s arena: a triangle node (`two` parts: its own chain) and a
/// marker node that ends on GP0(1Fh). Returns the heads of the first and the marker part.
fn build_chain(side: usize, f: u32, y: u16, two: bool) -> (*const u32, *const u32) {
    // SAFETY: the caller waited for `wait_arena_free`, so no walk reads this side; only raw
    // pointers into the static are formed.
    unsafe {
        let arena = addr_of_mut!(ARENAS[side]).cast::<u32>();
        let marker = arena.add(8);
        let shade = (f * 7) & 0xFF;
        let words = [
            0x3000_0000 | shade,
            pack(0, 0),
            shade << 8,
            pack(300, 0),
            shade << 16,
            pack(0, 200),
        ];
        let link = if two { 0x00FF_FFFF } else { marker as u32 & 0x00FF_FFFF };
        arena.write((6 << 24) | link);
        for (i, word) in words.iter().enumerate() {
            arena.add(1 + i).write(*word);
        }
        let (r, g, b) = marker_colour(f);
        let draw_done = psx_gpu::chain::DRAW_DONE_NODE.as_ptr() as u32 & 0x00FF_FFFF;
        marker.write((3 << 24) | draw_done);
        marker.add(1).write(psx_hw::gpu::gp0::fill_rect(r, g, b));
        marker.add(2).write(psx_hw::gpu::pack_xy(MARKER_X, y));
        marker.add(3).write(psx_hw::gpu::pack_xy(16, 16));
        (arena.cast_const(), marker.cast_const())
    }
}

const fn pack(x: u32, y: u32) -> u32 {
    x | (y << 16)
}

/// Spin for a frame-dependent stretch: none, a third of a VBlank, two thirds, or a whole one.
fn load(f: u32) {
    let start = interrupts::vblank_count();
    for _ in 0..(f % 4) * 40_000 {
        core::hint::spin_loop();
        if interrupts::vblank_count().wrapping_sub(start) > 1 {
            break;
        }
    }
}

/// The four records.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(RECORD);
    // SAFETY: plain statics; the hook is idle (`RUNNING` clear).
    let hooked = unsafe {
        if !read_volatile(addr_of!(HOOKED)) {
            write_volatile(addr_of_mut!(HOOKED), dispatch::on_vblank(hook).is_ok());
        }
        read_volatile(addr_of!(HOOKED))
    };
    let old_cap = present::cap();
    let old_timer1 = timers::mode(Timer::Timer1);
    present::wait_idle();
    timers::set_mode(Timer::Timer1, TIMER1_LINES_SINCE_VBLANK);
    present::set_cap(PresentCap::UNCAPPED);
    let mut buffers = DoubleBuffer::new(Resolution::R320X240);
    let mut presenter = Presenter::new(Some((8, 8, 24)));
    let before = present::stats();
    let fallbacks_before = interrupts::draw_done_fallback_count();
    let mut publish = [0u32; FRAMES];
    let mut parts = 0u32;
    let mut last_side_y = [0u16; 2];
    // SAFETY: the hook reads these only once `RUNNING` is set below.
    unsafe {
        for f in 0..=FRAMES {
            write_volatile(addr_of_mut!(FIRST_PART[f]), u32::MAX);
        }
        for value in [&raw mut STARTED, &raw mut TEARS, &raw mut LINE_MAX, &raw mut LAST_FLIPS] {
            write_volatile(value, 0);
        }
        write_volatile(addr_of_mut!(LINE_MIN), u32::MAX);
        write_volatile(addr_of_mut!(KICK_BASE), before.kick_count);
        write_volatile(addr_of_mut!(FLIP_BASE), flips_total());
        write_volatile(addr_of_mut!(RUNNING), hooked);
    }
    let mut lines_per_frame = 0u32;
    for (f, published) in publish.iter_mut().enumerate() {
        let frame = f as u32;
        present::wait_arena_free();
        load(frame);
        lines_per_frame = lines_per_frame.max(u32::from(timers::counter(Timer::Timer1)));
        let (_, y) = buffers.draw_origin();
        last_side_y[f & 1] = y;
        // SAFETY: the hook reads this entry only once kicks pass it.
        unsafe { write_volatile(addr_of_mut!(FIRST_PART[f]), parts) };
        if f % 5 == 4 {
            // Drawn through the port: the clear runs the guard, which waits for the last flip.
            probe_gpu!(gpu);
            gpu.draw(&FillRect::new((0, y), (320, 240), (24, 8, 8)));
            gpu.draw(&FillRect::new((MARKER_X, y), (16, 16), marker_colour(frame)));
            presenter.present_drawn(&mut buffers);
        } else {
            let two = f % 3 == 1;
            let (first, marker) = build_chain(f & 1, frame, y, two);
            // SAFETY: the arena is static and not rebuilt before `wait_arena_free` frees this side two
            // frames from now; the triangle node holds no GP0(1Fh) and the marker node ends on it.
            unsafe {
                if two {
                    presenter.submit(&mut buffers, first, Part::More);
                    presenter.submit(&mut buffers, marker, Part::Final);
                    parts += 2;
                } else {
                    presenter.submit(&mut buffers, first, Part::Final);
                    parts += 1;
                }
            }
        }
        *published = interrupts::vblank_count();
    }
    present::wait_idle();
    interrupts::wait_vblank();
    interrupts::wait_vblank();
    // SAFETY: the hook stops at `RUNNING` clear; plain statics read afterwards.
    let (tears, line_min, line_max, flip_ticks) = unsafe {
        write_volatile(addr_of_mut!(RUNNING), false);
        (
            read_volatile(addr_of!(TEARS)),
            read_volatile(addr_of!(LINE_MIN)),
            read_volatile(addr_of!(LINE_MAX)),
            read_volatile(addr_of!(FLIP_TICK)),
        )
    };
    let after = present::stats();
    let fallbacks = interrupts::draw_done_fallback_count().wrapping_sub(fallbacks_before);
    let flipped = flips_total().wrapping_sub(unsafe { read_volatile(addr_of!(FLIP_BASE)) });
    let kicks = after.kick_count.wrapping_sub(before.kick_count);
    let thread_kicks = after.thread_kick_count.wrapping_sub(before.thread_kick_count);
    let skips = after.skip_count.wrapping_sub(before.skip_count);
    let recoveries = after.recovery_count.wrapping_sub(before.recovery_count);

    // A frame waited past its edge when it flipped later than two edges after its submit and later
    // than the edge after the frame before.
    let mut late = 0u32;
    let mut worst = 0u32;
    for f in 0..FRAMES.min(flipped as usize) {
        let delay = flip_ticks[f].wrapping_sub(publish[f]);
        worst = worst.max(delay);
        if f > 0 && flip_ticks[f] > (publish[f] + 2).max(flip_ticks[f - 1] + 1) {
            late += 1;
        }
    }
    // The last two frames' markers, each in its own buffer.
    let mut bad_markers = 0u32;
    for f in [FRAMES - 2, FRAMES - 1] {
        let pixel = gpu_read_word_at(MARKER_X, last_side_y[f & 1]) & 0x7FFF;
        if pixel != colour_15(marker_colour(f as u32)) {
            bad_markers += 1;
        }
    }

    present::set_cap(old_cap);
    timers::set_mode(Timer::Timer1, old_timer1 & 0x03FF);
    probe_gpu!(gpu);
    gpu.set_draw_area((0, 0), (1023, 511));
    gpu.set_draw_offset((0, 0));

    let summary = [
        (" flipped=", flipped),
        (" parts=", parts),
        (" kicks=", kicks),
        (" thread=", thread_kicks),
        (" skips=", skips),
        (" first_line=", line_min),
        (" last_line=", line_max),
        (" lines=", lines_per_frame),
        (" tears=", tears),
        (" recoveries=", recoveries),
        (" markers_bad=", bad_markers),
        (" late=", late),
        (" worst=", worst),
        (" fallbacks=", fallbacks),
        (" hooked=", u32::from(hooked)),
    ];
    tty::print("hardware-tests: present-path");
    for (label, value) in summary {
        tty::print(label);
        let mut buffer = [0u8; 10];
        tty::print(crate::console_tests::number(&mut buffer, value));
    }
    tty::println("");
    let health = (tears.min(15) << 8) | (recoveries.min(15) << 4) | bad_markers.min(15);
    let health = if hooked { health } else { 0xFFFF };
    for (case, (a, b, c)) in [
        (flipped, kicks, health),
        (
            if line_min == u32::MAX { 0 } else { line_min },
            line_max,
            lines_per_frame,
        ),
        (thread_kicks, kicks.wrapping_sub(thread_kicks), skips),
        (late, worst, fallbacks),
    ]
    .into_iter()
    .enumerate()
    {
        push_timing_record(records, next, record(RECORD + case as u16, a, b, c));
    }
}
