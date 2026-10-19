// SPDX-License-Identifier: GPL-2.0-or-later
//! GPU LIST NODE SIZES (v2.8): where does a linked-list node start losing data?
//!
//! On silicon a node longer than the GPU's 16-word GP0 FIFO can lose its head
//! when channel 2 walks it into a busy GPU, and the closing GP0(1Fh) never
//! arrives (WipEout's 41-word title node, Celeste 0.2.3). v1.24 case 0xE3
//! drew 24-word nodes fine, so the threshold lies between 24 and 41. This
//! step finds it.
//!
//! For each node size N in [`SIZES`], twice (the GPU busy with two large
//! triangles when the node arrives, and idle), one linked list is walked by
//! DMA channel 2:
//!
//! ```text
//! [busy triangle][busy triangle]  (busy trials only)
//! [N-word node: flat 6x4 rectangles, 3 words each, GP0 NOPs for the rest]
//! [GP0(1Fh) node]
//! ```
//!
//! Afterwards (every wait bounded by a clock deadline and a pass count): did
//! the GP0(1Fh) interrupt request arrive, how long did it take, did the
//! channel finish, where did its address stop, and which rectangles drew in
//! the right colour at the right place (read back from VRAM). The GPU is then
//! recovered (DMA aborted, command parser reset) before the next trial.
//!
//! Records from `0xC00`: per trial `0xC00 + 4 * trial` and `+ 1`
//! (trial = size index * 2 + idle):
//! first: flags, rectangles drawn mask low 16 bits, mask high bits;
//!   flags: bit 0 the IRQ arrived, bit 1 the channel finished, bit 2 every
//!   rectangle drew, bit 3 the GPU answered after recovery, bit 4 idle trial,
//!   bits 8 to 15 the node's size in words;
//! second: clocks to the IRQ (low, high; 0xFFFF 0xFFFF when it never came),
//!   words the address advanced past the head.

use crate::console_tests::record;
use crate::v24::{self, xy, Clock32};
use crate::{bounds, ui, TimingRecord};
use core::ptr::{addr_of, addr_of_mut, write_volatile};
use psx_font::FontAtlas;
use psx_io::dma;
use psx_io::gpu as gpu_io;
use psx_rt::tty;

pub(crate) const LL_RECORD: u16 = 0xC00;
/// Node sizes in words.
pub(crate) const SIZES: [u32; 9] = [16, 20, 24, 28, 32, 36, 41, 48, 64];
const TRIALS: usize = SIZES.len() * 2;
/// Rectangles at most: 64 / 3.
const MAX_RECTS: usize = 21;
const RECT_W: u32 = 8;
const RECT_Y: u32 = 504;
const RECT_H: u32 = 4;
const LIST_LEN: usize = 160;
static mut LIST: [u32; LIST_LEN] = [0; LIST_LEN];

/// A clock deadline for the list to finish (system clocks, 0.6 s) and a pass
/// count that bounds the loop whatever the timer does.
const DEADLINE: u32 = 20_000_000;
const PASSES: u32 = 1_500_000;

const KICK_LIST: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;

/// What a trial saw.
#[derive(Copy, Clone)]
pub(crate) struct Trial {
    size: u32,
    idle: bool,
    irq: bool,
    finished: bool,
    gpu_ok: bool,
    mask: u32,
    rects: u32,
    clocks: u32,
    advanced: u32,
}

const NONE: Trial = Trial {
    size: 0,
    idle: false,
    irq: false,
    finished: false,
    gpu_ok: false,
    mask: 0,
    rects: 0,
    clocks: u32::MAX,
    advanced: 0,
};

pub(crate) static mut TABLE: [Trial; TRIALS] = [NONE; TRIALS];
static mut TABLE_LEN: usize = 0;

/// The pixel rectangle `index` draws: green full, red the index plus one.
const fn pixel(index: u32) -> u32 {
    0x3E0 | (index + 1)
}

/// Build the list. Returns the head and the number of rectangles in the node.
fn build(size: u32, busy: bool) -> (u32, u32) {
    let list = addr_of_mut!(LIST) as *mut u32;
    let mut at = 0usize;
    let mut node = |words: &[u32], last: bool| {
        let next = if last {
            0x00FF_FFFF
        } else {
            // SAFETY: LIST holds every node of the largest trial.
            unsafe { list.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF }
        };
        // SAFETY: as above.
        unsafe {
            write_volatile(list.add(at), ((words.len() as u32) << 24) | next);
            for (i, word) in words.iter().enumerate() {
                write_volatile(list.add(at + 1 + i), *word);
            }
        }
        at += 1 + words.len();
    };
    if busy {
        // Two large flat triangles off screen to the right: a few ms of drawing.
        node(
            &[0x2000_00FF, xy(512, 0), xy(1023, 0), xy(512, 255)],
            false,
        );
        node(
            &[0x2000_FF00, xy(1023, 0), xy(1023, 255), xy(512, 255)],
            false,
        );
    }
    let rects = size / 3;
    let mut words = [0u32; 64];
    for i in 0..rects {
        let base = (i * 3) as usize;
        // Flat variable-size rectangle: red channel carries the index.
        words[base] = 0x6000_F800 | ((i + 1) << 3);
        words[base + 1] = xy(i * RECT_W, RECT_Y);
        words[base + 2] = xy(6, RECT_H);
    }
    // The rest of the node is GP0 NOPs (zeros).
    node(&words[..size as usize], false);
    node(&[0x1F00_0000], true);
    (list as u32, rects)
}

/// Rectangles whose centre pixel is the right colour, as a mask, from a
/// one-row read-back of the strip.
fn read_mask(rects: u32) -> Option<u32> {
    let mut mask = 0u32;
    let mut word_index = 0u32;
    let ok = v24::read_rect(0, RECT_Y + 1, rects * RECT_W, 1, |word| {
        for half in 0..2 {
            let x = word_index * 2 + half;
            let value = if half == 0 { word & 0xFFFF } else { word >> 16 };
            let rect = x / RECT_W;
            if x % RECT_W == 3 && rect < rects && value == pixel(rect) {
                mask |= 1 << rect;
            }
        }
        word_index += 1;
    });
    ok.then_some(mask)
}

#[inline(never)]
fn trial(size: u32, idle: bool) -> Trial {
    let id = LL_RECORD + 4 * (SIZES.iter().position(|&s| s == size).unwrap_or(0) as u16 * 2 + idle as u16);
    bounds::record_start(id);
    // A known display and draw state whatever the steps before left: 320x240
    // progressive (an interlaced mode makes drawing skip the displayed field's
    // lines) and drawing to the display area allowed (E1 bit 10).
    gpu_io::write_display_control(0x0800_0001);
    v24::environment(1 << 10);
    // Clear the strip, then make sure the GPU is quiet.
    gpu_io::wait_command_ready();
    v24::send(&[0x0200_0000, xy(0, RECT_Y), xy(((MAX_RECTS as u32) * RECT_W).div_ceil(16) * 16, 8)]);
    let _ = v24::drain();
    let (head, rects) = build(size, !idle);
    gpu_io::write_display_control(0x0400_0002);
    dma::enable_channel(dma::Channel::Gpu);
    v24::ack_gpu_irq();
    let mut irq = false;
    let mut clocks = u32::MAX;
    // SAFETY: silicon probe: the transfer reads only the static list and
    // writes the GPU; the wait is bounded and the channel stopped below.
    let (finished, advanced) = unsafe {
        dma::raw::set_address(dma::Channel::Gpu, head);
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
        let mut clock = Clock32::start();
        dma::raw::set_control(dma::Channel::Gpu, KICK_LIST);
        let mut passes = bounds::scale(PASSES);
        let mut finished = false;
        while passes > 0 {
            passes -= 1;
            let now = clock.now();
            if !irq && gpu_io::status().bits() & v24::STAT_IRQ1 != 0 {
                irq = true;
                clocks = now;
            }
            if !dma::is_busy(dma::Channel::Gpu) {
                finished = true;
                if irq {
                    break;
                }
            }
            if now > DEADLINE {
                break;
            }
        }
        let address = dma::address(dma::Channel::Gpu);
        (finished, (address.wrapping_sub(head) & 0x00FF_FFFF) / 4)
    };
    // Recover: stop the channel, let the GPU finish what it holds, drop any
    // half-received command.
    dma::abort(dma::Channel::Gpu);
    v24::spin_cycles(300_000);
    v24::reset_command_buffer();
    gpu_io::write_display_control(0x0400_0000);
    v24::ack_gpu_irq();
    let ping = v24::drain();
    let mask = read_mask(rects);
    Trial {
        size,
        idle,
        irq,
        finished,
        gpu_ok: ping && mask.is_some(),
        mask: mask.unwrap_or(0),
        rects,
        clocks,
        advanced: advanced.min(0xFFFF),
    }
}

impl Trial {
    fn all_drawn(&self) -> bool {
        let full = if self.rects >= 32 { u32::MAX } else { (1u32 << self.rects) - 1 };
        self.mask == full
    }
}

fn push(out: &mut LlRecords, index: usize, t: &Trial) {
    let id = LL_RECORD + 4 * index as u16;
    let flags = t.irq as u32
        | (t.finished as u32) << 1
        | (t.all_drawn() as u32) << 2
        | (t.gpu_ok as u32) << 3
        | (t.idle as u32) << 4
        | t.size << 8;
    out.push(record(id, flags, t.mask & 0xFFFF, t.mask >> 16));
    let (lo, hi) = if t.clocks == u32::MAX {
        (0xFFFF, 0xFFFF)
    } else {
        (t.clocks & 0xFFFF, t.clocks >> 16)
    };
    out.push(record(id + 1, lo, hi, t.advanced));
}

pub(crate) struct LlRecords {
    items: [TimingRecord; TRIALS * 2],
    len: usize,
}

impl LlRecords {
    fn push(&mut self, r: TimingRecord) {
        if self.len < self.items.len() {
            self.items[self.len] = r;
            self.len += 1;
        }
    }

    pub(crate) fn as_slice(&self) -> &[TimingRecord] {
        &self.items[..self.len]
    }
}

fn dec(v: u32) -> ui::Line {
    let mut l = ui::Line::new();
    l.u(v);
    l
}

pub(crate) fn run(font: &FontAtlas) -> LlRecords {
    let mut out = LlRecords {
        items: [TimingRecord::pending(); TRIALS * 2],
        len: 0,
    };
    // SAFETY: single thread; plain statics.
    unsafe {
        TABLE_LEN = 0;
        *addr_of_mut!(TABLE) = [NONE; TRIALS];
    }
    let mut index = 0;
    for &size in SIZES.iter() {
        for idle in [false, true] {
            if bounds::skipping() {
                return out;
            }
            let mut tag = ui::Line::new();
            tag.s("NODE ").u(size).s(if idle { " IDLE GPU" } else { " BUSY GPU" });
            ui::detail(font, "LIST", tag.as_str());
            psx_io::diag::step(0xC00 + index as u32, "LIST NODE");
            let t = trial(size, idle);
            // SAFETY: single thread; plain statics.
            unsafe {
                (*addr_of_mut!(TABLE))[index] = t;
                TABLE_LEN = index + 1;
            }
            push(&mut out, index, &t);
            tty::print("hardware-tests: ll node ");
            tty::print(dec(size).as_str());
            tty::print(if idle { " idle irq=" } else { " busy irq=" });
            tty::print(if t.irq { "1" } else { "0" });
            tty::print(" drawn=");
            tty::print(dec(t.mask.count_ones()).as_str());
            tty::print("/");
            tty::print(dec(t.rects).as_str());
            tty::print("\n");
            let mut line = ui::Line::new();
            row_text(&mut line, &t);
            ui::live_line(
                font,
                (index as u32) % ui::LIVE_ROWS,
                line.as_str(),
                colour(&t),
            );
            ui::sub(index + 1, TRIALS);
            index += 1;
        }
    }
    out
}

fn good(t: &Trial) -> bool {
    t.irq && t.all_drawn()
}

fn colour(t: &Trial) -> (u8, u8, u8) {
    if good(t) {
        (150, 230, 160)
    } else {
        (255, 110, 110)
    }
}

fn row_text(line: &mut ui::Line, t: &Trial) {
    if t.size < 10 {
        line.s(" ");
    }
    line.u(t.size).s(if t.idle { " IDLE " } else { " BUSY " });
    line.s(if t.irq { "IRQ " } else { "NOIRQ " });
    line.u(t.mask.count_ones()).s("/").u(t.rects);
    line.s(if t.finished { " DMA" } else { " HUNG" });
    if !t.gpu_ok {
        line.s(" GPU?");
    }
}

pub(crate) fn page_count() -> usize {
    // SAFETY: single thread; plain static.
    usize::from(unsafe { TABLE_LEN } != 0)
}

/// The page, drawn every frame.
pub(crate) fn draw_page(font: &FontAtlas) {
    // SAFETY: single thread; plain statics.
    let (table, len) = unsafe { (&*addr_of!(TABLE), TABLE_LEN) };
    let head = (255, 232, 128);
    let plain = (176, 190, 210);
    font.draw_text(8, 6, "GPU LIST NODE SIZES", head);
    font.draw_text(8, 17, "SIZE GPU  IRQ  RECTS DRAWN  DMA", plain);
    let mut y = 30;
    for t in table.iter().take(len) {
        let mut row = ui::Line::new();
        row_text(&mut row, t);
        font.draw_text(8, y, row.as_str(), colour(t));
        y += 9;
    }
    y += 6;
    let first_bad = table.iter().take(len).filter(|t| !good(t)).map(|t| t.size).min();
    let mut line = ui::Line::new();
    match first_bad {
        Some(size) => {
            line.s("DATA OR IRQ LOST FROM ").u(size).s(" WORDS");
            font.draw_text(8, y, line.as_str(), (255, 110, 110));
        }
        None => {
            line.s("NO LOSS UP TO ").u(SIZES[SIZES.len() - 1]).s(" WORDS");
            font.draw_text(8, y, line.as_str(), (150, 230, 160));
        }
    }
}
