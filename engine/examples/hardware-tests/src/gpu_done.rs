// SPDX-License-Identifier: GPL-2.0-or-later
//! GPU DONE SIGNAL ONLY (v2.10): does GP0(1Fh) raise GPUSTAT bit 24, and
//! I_STAT bit 1, every time it is sent?
//!
//! Why: on an FPGA PSX core a game that ends its frame with a GP0(1Fh) node and
//! waits for bit 24 ran at 2 FPS, while a retail PS1 and the emulator never
//! miss. The core's IRQ pulse is one clk2x wide when the word is popped on one
//! clk2x phase and two wide on the other, and only the wide one crosses into
//! the clk1x process that sets bit 24 and I_STAT. Idle-GPU pops would then
//! always land on the bad phase. This step measures it, with nothing else in
//! the way.
//!
//! Every row repeats [`REPEATS`] attempts. An attempt acknowledges the GPU
//! interrupt (GP1 02h) and I_STAT bit 1, sends a script through GP0 or through
//! a DMA channel 2 linked list, and polls GPUSTAT and I_STAT until bit 24
//! rises or [`DEADLINE`] clocks pass. Per row it reports how many attempts saw
//! bit 24, how many saw I_STAT bit 1 (and how many only I_STAT, bit 24
//! never), the median polls and microseconds to bit 24, and, at each timeout,
//! whether GPUSTAT bits 26, 27 and 28 and the channel said the GPU had in fact
//! finished.
//!
//! Nothing here is a record: the screen is the result, in decimal, and the
//! same numbers go to the TTY. Pages: 0 the big summary, 1 the per-row
//! statistics, 2 the state seen at each timeout.

use crate::gpu_probes::{CLUT_A, DEPTH_4BPP, DRAW_X, DRAW_Y, TEX_PAGE_A};
use crate::ui;
use crate::v24::{self, xy, Clock32};
use core::ptr::{addr_of, addr_of_mut, write_volatile};
use psx_font::FontAtlas;
use psx_io::dma;
use psx_io::gpu as gpu_io;
use psx_io::irq;
use psx_rt::tty;

/// Attempts per row.
pub(crate) const REPEATS: usize = 24;
pub(crate) const ROWS: usize = 18;
/// System clocks an attempt may wait for bit 24 (about 118 ms, seven frames).
const DEADLINE: u32 = 4_000_000;
/// Passes after which a poll loop gives up whatever the timer says.
const PASS_CAP: u32 = 1_000_000;
/// System clocks the GPU gets to go quiet between attempts.
const QUIET_DEADLINE: u32 = 6_000_000;

const STAT_IRQ: u32 = 1 << 24;
const STAT_CMD: u32 = 1 << 26;
const STAT_VRAM: u32 = 1 << 27;
const STAT_DMA: u32 = 1 << 28;
const ISTAT_GPU: u32 = 1 << 1;
const KICK_LIST: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;

const IRQ_CMD: u32 = 0x1F00_0000;
const NOP: u32 = 0;
/// Big textured triangle: wide and tall enough to take milliseconds.
const BIG_W: u32 = 256;
const BIG_H: u32 = 100;
const BIG_COUNT: usize = 16;
const SCRIPT_WORDS: usize = 256;
const LIST_WORDS: usize = 320;
static mut LIST: [u32; LIST_WORDS] = [0; LIST_WORDS];

#[derive(Copy, Clone, PartialEq, Eq)]
enum Transport {
    /// Words written one by one to GP0, paced on bit 26 when the script is long.
    Port,
    /// Words written to GP0 back to back, never paced (idle GPU only).
    PortRaw,
    /// One DMA channel 2 linked list; a node ends at every cut.
    Dma,
}

struct Script {
    words: [u32; SCRIPT_WORDS],
    /// A node ends after this word (DMA only).
    cut: [bool; SCRIPT_WORDS],
    len: usize,
}

impl Script {
    const fn new() -> Self {
        Self {
            words: [0; SCRIPT_WORDS],
            cut: [false; SCRIPT_WORDS],
            len: 0,
        }
    }

    fn word(&mut self, w: u32) {
        if self.len < SCRIPT_WORDS {
            self.words[self.len] = w;
            self.len += 1;
        }
    }

    fn cut(&mut self) {
        if self.len > 0 {
            self.cut[self.len - 1] = true;
        }
    }

    fn tri(&mut self) {
        let (x, y) = (DRAW_X, DRAW_Y);
        self.word(0x2480_8080);
        self.word(xy(x, y));
        self.word(CLUT_A << 16);
        self.word(xy(x + BIG_W, y));
        self.word(((TEX_PAGE_A | DEPTH_4BPP) << 16) | 63);
        self.word(xy(x, y + BIG_H));
        self.word(63 << 8);
    }

    fn big_chain(&mut self) {
        for _ in 0..BIG_COUNT {
            self.tri();
            self.cut();
        }
    }

    fn rect(&mut self, w: u32, h: u32) {
        self.word(0x6000_80FF);
        self.word(xy(DRAW_X + 300, DRAW_Y));
        self.word(xy(w, h));
    }
}

/// One row of the table: its short name, transport, and script.
struct Row {
    name: &'static str,
    transport: Transport,
}

const TABLE_A: [Row; ROWS] = [
    Row { name: "PORT IDLE", transport: Transport::Port },
    Row { name: "PORT NOP", transport: Transport::Port },
    Row { name: "PORT X2", transport: Transport::Port },
    Row { name: "DMA IDLE", transport: Transport::Dma },
    Row { name: "DMA BIG", transport: Transport::Dma },
    Row { name: "BIG X2", transport: Transport::Dma },
    Row { name: "BIG X3", transport: Transport::Dma },
    Row { name: "BIG X4", transport: Transport::Dma },
    Row { name: "BIG X2 NODES", transport: Transport::Dma },
    Row { name: "BIG X4 NODES", transport: Transport::Dma },
    Row { name: "DMA SMALL", transport: Transport::Dma },
    Row { name: "SPACED", transport: Transport::Dma },
    Row { name: "PORT BIG", transport: Transport::Port },
    Row { name: "PORT R1X1", transport: Transport::Port },
    Row { name: "PORT R2X2", transport: Transport::Port },
    Row { name: "PORT R5X1", transport: Transport::Port },
    Row { name: "PORT R16X4", transport: Transport::Port },
    Row { name: "PORT FILL", transport: Transport::Port },
];

const ROWS_B: usize = 11;
const TABLE_B: [Row; ROWS_B] = [
    Row { name: "FIX PORT IDLE", transport: Transport::Port },
    Row { name: "FIX DMA IDLE", transport: Transport::Dma },
    Row { name: "FIX BIG16", transport: Transport::Dma },
    Row { name: "FIX BIG15", transport: Transport::Dma },
    Row { name: "FIX PORT BIG", transport: Transport::Port },
    Row { name: "COPY 1X1", transport: Transport::Dma },
    Row { name: "COPY 3X3", transport: Transport::Dma },
    Row { name: "COPY 6X4", transport: Transport::Dma },
    Row { name: "COPY 8X8", transport: Transport::Dma },
    Row { name: "COPY 16X4", transport: Transport::Dma },
    Row { name: "COPY 40X6", transport: Transport::Dma },
];

const ROWS_C: usize = 18;
const TABLE_C: [Row; ROWS_C] = [
    Row { name: "FILL 16X2", transport: Transport::Dma },
    Row { name: "FILL 16X3", transport: Transport::Dma },
    Row { name: "FILL 16X4", transport: Transport::Dma },
    Row { name: "FILL 16X5", transport: Transport::Dma },
    Row { name: "FILL 16X6", transport: Transport::Dma },
    Row { name: "FILL 16X7", transport: Transport::Dma },
    Row { name: "FILL 16X8", transport: Transport::Dma },
    Row { name: "BIG FILL X1", transport: Transport::Dma },
    Row { name: "BIG FILL X2", transport: Transport::Dma },
    Row { name: "BIG FILL X3", transport: Transport::Dma },
    Row { name: "BIG FILL X4", transport: Transport::Dma },
    Row { name: "BIG FILL X5", transport: Transport::Dma },
    Row { name: "BIG FILL X6", transport: Transport::Dma },
    Row { name: "BIG FILL X7", transport: Transport::Dma },
    Row { name: "BIG FILL X8", transport: Transport::Dma },
    Row { name: "SHIP DMA IDLE", transport: Transport::Dma },
    Row { name: "SHIP DMA BIG", transport: Transport::Dma },
    Row { name: "SHIP PORT RAW", transport: Transport::PortRaw },
];

/// Which table is on show: 0 the probe, 1 the fix candidates, 2 fills and the shipped node.
static mut SET: u8 = 0;

fn table() -> &'static [Row] {
    // SAFETY: single thread; plain static.
    match unsafe { SET } {
        0 => &TABLE_A,
        1 => &TABLE_B,
        _ => &TABLE_C,
    }
}

fn row_count() -> usize {
    table().len()
}

/// Sizes of the self-copies in the closing sequence, in order.
pub(crate) const FIX_COPIES: [(u32, u32); 4] = [(16, 4), (6, 4), (40, 6), (8, 8)];
/// Where the self-copies sit: the corner of VRAM, nothing drawn or displayed.
pub(crate) const COPY_X: u32 = 976;
pub(crate) const COPY_Y: u32 = 500;

fn script(row: usize) -> Script {
    match unsafe { SET } {
        1 => return script_b(row),
        2 => return script_c(row),
        _ => {}
    }
    let mut s = Script::new();
    match row {
        // Idle GPU, GP0 port.
        0 => s.word(IRQ_CMD),
        1 => {
            s.word(NOP);
            s.word(IRQ_CMD);
        }
        2 => {
            s.word(IRQ_CMD);
            s.word(NOP);
            s.word(IRQ_CMD);
        }
        // Idle GPU, a one-word list.
        3 => s.word(IRQ_CMD),
        // A big chain, then the closing node.
        4 => {
            s.big_chain();
            s.word(IRQ_CMD);
        }
        // The closing node sent 2, 3 and 4 times, NOPs between, one node.
        5 | 6 | 7 => {
            s.big_chain();
            let sends = row - 3;
            for i in 0..sends {
                if i > 0 {
                    s.word(NOP);
                }
                s.word(IRQ_CMD);
            }
        }
        // The same, one node per word.
        8 | 9 => {
            s.big_chain();
            let sends = if row == 8 { 2 } else { 4 };
            for i in 0..sends {
                if i > 0 {
                    s.word(NOP);
                    s.cut();
                }
                s.word(IRQ_CMD);
                s.cut();
            }
        }
        // A short draw, then the closing node.
        10 => {
            s.rect(6, 4);
            s.cut();
            s.word(IRQ_CMD);
        }
        // Closing words spaced by draws of different lengths.
        11 => {
            s.big_chain();
            for (w, h) in [(1, 1), (3, 3), (8, 1), (40, 6)] {
                s.word(IRQ_CMD);
                s.cut();
                s.rect(w, h);
                s.cut();
            }
            s.word(IRQ_CMD);
        }
        // One big triangle through the port (7 words fit the 16-word FIFO).
        12 => {
            s.tri();
            s.word(IRQ_CMD);
        }
        // A tiny draw of a known length, then the request: where the word
        // pops depends on when the draw ends.
        13..=16 => {
            let (w, h) = [(1, 1), (2, 2), (5, 1), (16, 4)][row - 13];
            s.rect(w, h);
            s.word(IRQ_CMD);
        }
        // A VRAM fill (its own timing), then the request.
        _ => {
            s.word(0x0200_0000);
            s.word(xy(DRAW_X + 300, DRAW_Y + 32));
            s.word(xy(16, 1));
            s.word(IRQ_CMD);
        }
    }
    s
}

impl Script {
    /// A VRAM-to-VRAM copy of a block onto itself: no pixel changes.
    fn self_copy(&mut self, w: u32, h: u32) {
        self.word(0x8000_0000);
        self.word(xy(COPY_X, COPY_Y));
        self.word(xy(COPY_X, COPY_Y));
        self.word(xy(w, h));
    }

    /// The closing sequence a game sends instead of one 1Fh: the request, then
    /// a self-copy and another request, for each size. Bit 24 is sticky, so
    /// only the first request that lands matters.
    fn fix_sequence(&mut self, cut: bool) {
        self.word(IRQ_CMD);
        if cut {
            self.cut();
        }
        for (w, h) in FIX_COPIES {
            self.self_copy(w, h);
            if cut {
                self.cut();
            }
            self.word(IRQ_CMD);
            if cut {
                self.cut();
            }
        }
    }
}

fn script_b(row: usize) -> Script {
    let mut s = Script::new();
    // Mask off, as a game's reset leaves it: a copy must not set or check it.
    s.word(0xE600_0000);
    match row {
        0 => s.fix_sequence(false),
        1 => s.fix_sequence(true),
        2 | 3 => {
            s.cut();
            for _ in 0..(if row == 2 { BIG_COUNT } else { BIG_COUNT - 1 }) {
                s.tri();
                s.cut();
            }
            s.fix_sequence(true);
        }
        4 => {
            s.tri();
            s.fix_sequence(false);
        }
        _ => {
            let (w, h) = [(1, 1), (3, 3), (6, 4), (8, 8), (16, 4), (40, 6)][row - 5];
            s.cut();
            s.word(IRQ_CMD);
            s.cut();
            s.self_copy(w, h);
            s.cut();
            s.word(IRQ_CMD);
        }
    }
    s
}

/// The scratch block of the SDK's draw-done sequence (psx-hw
/// `gp0::DRAW_DONE_SCRATCH`, branch lane/fpga-irq-shared-fix): 16 x 8 pixels.
const SCRATCH_X: u32 = 1008;
const SCRATCH_Y: u32 = 504;
/// The words of the SDK's `gp0::DRAW_DONE_SEQUENCE` (GP0(1Fh), a fill of the
/// scratch block, three times, then GP0(1Fh): 13 words, one node), written out
/// again here because this repo's psx-hw is a hydrated copy that does not have
/// it; the TTY prints them so a log can be compared with the SDK's.
const SHIPPED_NODE: [u32; 13] = {
    let at = xy(SCRATCH_X, SCRATCH_Y);
    let size = xy(16, 8);
    [
        IRQ_CMD, 0x0200_0000, at, size, IRQ_CMD, 0x0200_0000, at, size, IRQ_CMD, 0x0200_0000, at,
        size, IRQ_CMD,
    ]
};

/// A fill of `h` lines of the scratch block: GP0(02h) ignores the draw area
/// and the mask bits and reads no VRAM, so it changes nothing but the block.
fn fill_scratch(s: &mut Script, h: u32) {
    s.word(0x0200_0000);
    s.word(xy(SCRATCH_X, SCRATCH_Y));
    s.word(xy(16, h));
}

fn script_c(row: usize) -> Script {
    let mut s = Script::new();
    match row {
        0..=14 => {
            let (big, h) = if row < 7 { (false, row as u32 + 2) } else { (true, row as u32 - 6) };
            if big {
                s.big_chain();
            }
            s.word(IRQ_CMD);
            s.cut();
            fill_scratch(&mut s, h);
            s.cut();
            s.word(IRQ_CMD);
        }
        _ => {
            // The words that ship: the shared node, as one node or as a
            // burst, after nothing or after 16 big triangles.
            if row == 16 {
                s.big_chain();
            }
            for w in SHIPPED_NODE {
                s.word(w);
            }
        }
    }
    s
}

/// Build the DMA list for a script. Returns the head address.
fn build_list(s: &Script) -> u32 {
    let list = addr_of_mut!(LIST) as *mut u32;
    let mut at = 0usize;
    let mut start = 0usize;
    while start < s.len {
        let mut end = start;
        while end + 1 < s.len && !s.cut[end] && end + 1 - start < 64 {
            end += 1;
        }
        end += 1; // exclusive
        let words = end - start;
        let last = end >= s.len;
        // SAFETY: LIST holds every node of the largest script.
        unsafe {
            let next = if last {
                0x00FF_FFFF
            } else {
                list.add(at + 1 + words) as u32 & 0x00FF_FFFF
            };
            write_volatile(list.add(at), ((words as u32) << 24) | next);
            for (i, w) in s.words[start..end].iter().enumerate() {
                write_volatile(list.add(at + 1 + i), *w);
            }
        }
        at += 1 + words;
        start = end;
    }
    list as u32
}

/// One attempt's observations.
#[derive(Copy, Clone)]
struct Attempt {
    seen24: bool,
    seen_istat: bool,
    polls: u32,
    clocks: u32,
    /// State at the timeout, when there was one.
    b26: bool,
    b27: bool,
    b28: bool,
    dma_busy: bool,
    quiet_before: bool,
}

/// GPUSTAT bits 26 and 28 up (bit 27 is only up during a VRAM read) and the
/// channel idle, within a bound.
fn quiet() -> bool {
    let mut clock = Clock32::start();
    let mut passes = PASS_CAP;
    while passes > 0 {
        passes -= 1;
        let st = gpu_io::status().bits();
        if st & (STAT_CMD | STAT_DMA) == (STAT_CMD | STAT_DMA)
            && !dma::is_busy(dma::Channel::Gpu)
        {
            return true;
        }
        if clock.now() > QUIET_DEADLINE {
            break;
        }
    }
    false
}

#[inline(never)]
fn attempt(row: usize, s: &Script, head: u32) -> Attempt {
    let transport = table()[row].transport;
    // A GPU that never went quiet gets a command-buffer reset; the attempt
    // still runs and reports that it was not clean.
    let quiet_before = quiet();
    if !quiet_before {
        dma::abort(dma::Channel::Gpu);
        v24::reset_command_buffer();
    }
    v24::environment(1 << 10);
    gpu_io::wait_command_ready();
    gpu_io::write_display_control(match transport {
        Transport::Port | Transport::PortRaw => 0x0400_0000,
        Transport::Dma => 0x0400_0002,
    });
    dma::enable_channel(dma::Channel::Gpu);
    v24::ack_gpu_irq();
    irq::acknowledge(ISTAT_GPU);
    // Both acknowledges have to reach the registers before the clock starts.
    for _ in 0..64 {
        let _ = gpu_io::status();
    }
    let mut out = Attempt {
        seen24: false,
        seen_istat: false,
        polls: 0,
        clocks: 0,
        b26: false,
        b27: false,
        b28: false,
        dma_busy: false,
        quiet_before,
    };
    let mut clock = Clock32::start();
    match transport {
        Transport::Port | Transport::PortRaw => {
            // A long script is paced: bit 26 before each word, so the 16-word
            // FIFO never overflows behind a big draw.
            let paced = s.len > 12 && transport == Transport::Port;
            for w in &s.words[..s.len] {
                if paced {
                    gpu_io::wait_command_ready();
                }
                gpu_io::write_command(*w);
            }
        }
        Transport::Dma => {
            // SAFETY: silicon probe: the transfer reads only the static list
            // and writes the GPU; the wait is bounded and the channel is
            // stopped below.
            unsafe {
                dma::raw::set_address(dma::Channel::Gpu, head);
                dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
                dma::raw::set_control(dma::Channel::Gpu, KICK_LIST);
            }
        }
    }
    let mut passes = PASS_CAP;
    loop {
        let st = gpu_io::status().bits();
        let pending = irq::pending();
        out.polls += 1;
        if pending & ISTAT_GPU != 0 {
            out.seen_istat = true;
        }
        if st & STAT_IRQ != 0 {
            out.seen24 = true;
            out.clocks = clock.now();
            break;
        }
        passes -= 1;
        let now = clock.now();
        if now > DEADLINE || passes == 0 {
            out.clocks = now;
            out.b26 = st & STAT_CMD != 0;
            out.b27 = st & STAT_VRAM != 0;
            out.b28 = st & STAT_DMA != 0;
            out.dma_busy = dma::is_busy(dma::Channel::Gpu);
            break;
        }
    }
    // One more look at I_STAT after bit 24 (it is latched a clock apart).
    if irq::pending() & ISTAT_GPU != 0 {
        out.seen_istat = true;
    }
    dma::abort(dma::Channel::Gpu);
    gpu_io::write_display_control(0x0400_0000);
    v24::ack_gpu_irq();
    irq::acknowledge(ISTAT_GPU);
    out
}

/// One row's numbers.
#[derive(Copy, Clone)]
pub(crate) struct Stats {
    pub(crate) done: bool,
    pub(crate) ok: u32,
    pub(crate) istat: u32,
    /// I_STAT bit 1 rose and bit 24 never did.
    pub(crate) istat_only: u32,
    pub(crate) timeouts: u32,
    pub(crate) b26: u32,
    pub(crate) b27: u32,
    pub(crate) b28: u32,
    pub(crate) dma_busy: u32,
    pub(crate) unquiet: u32,
    pub(crate) med_polls: u32,
    pub(crate) med_clocks: u32,
}

const EMPTY: Stats = Stats {
    done: false,
    ok: 0,
    istat: 0,
    istat_only: 0,
    timeouts: 0,
    b26: 0,
    b27: 0,
    b28: 0,
    dma_busy: 0,
    unquiet: 0,
    med_polls: 0,
    med_clocks: 0,
};

static mut RESULTS: [Stats; ROWS] = [EMPTY; ROWS];
static mut RESULT_ROWS: usize = 0;

fn median(values: &mut [u32]) -> u32 {
    // Insertion sort: at most REPEATS values.
    for i in 1..values.len() {
        let mut j = i;
        while j > 0 && values[j - 1] > values[j] {
            values.swap(j - 1, j);
            j -= 1;
        }
    }
    if values.is_empty() {
        0
    } else {
        values[values.len() / 2]
    }
}

fn run_row(row: usize) -> Stats {
    let s = script(row);
    let head = if table()[row].transport == Transport::Dma {
        build_list(&s)
    } else {
        0
    };
    let mut stats = EMPTY;
    let mut polls = [0u32; REPEATS];
    let mut clocks = [0u32; REPEATS];
    let mut good = 0usize;
    for _ in 0..REPEATS {
        let a = attempt(row, &s, head);
        if !a.quiet_before {
            stats.unquiet += 1;
        }
        if a.seen_istat {
            stats.istat += 1;
        }
        if a.seen24 {
            stats.ok += 1;
            polls[good] = a.polls;
            clocks[good] = a.clocks;
            good += 1;
        } else {
            stats.timeouts += 1;
            if a.seen_istat {
                stats.istat_only += 1;
            }
            stats.b26 += a.b26 as u32;
            stats.b27 += a.b27 as u32;
            stats.b28 += a.b28 as u32;
            stats.dma_busy += a.dma_busy as u32;
        }
    }
    stats.med_polls = median(&mut polls[..good]);
    stats.med_clocks = median(&mut clocks[..good]);
    stats.done = true;
    stats
}

fn us(clocks: u32) -> u32 {
    // 33.8688 clocks a microsecond.
    clocks / 34
}

/// Run every row. Draws progress on the run screen; results stay in statics.
pub(crate) fn run(font: &FontAtlas, set: u8) {
    // SAFETY: single thread; plain static.
    unsafe { SET = set };
    // SAFETY: single thread; plain statics.
    unsafe {
        *addr_of_mut!(RESULTS) = [EMPTY; ROWS];
        RESULT_ROWS = 0;
    }
    crate::gpu_probes::upload_textures();
    tty::println("hardware-tests: gpu-done begins");
    if unsafe { SET } == 2 {
        for half in 0..4 {
            let mut l = ui::Line::new();
            l.s("hardware-tests: gpu-done shipped node ").u(half as u32);
            for w in &SHIPPED_NODE[half * 4..(half * 4 + 4).min(13)] {
                l.s(" ").hex(*w, 8);
            }
            tty::println(l.as_str());
        }
    }
    for row in 0..row_count() {
        let mut tag = ui::Line::new();
        tag.s("ROW ").u(row as u32 + 1).s(" OF ").u(row_count() as u32).s(" ").s(table()[row].name);
        ui::detail(font, "DONE", tag.as_str());
        let stats = run_row(row);
        // SAFETY: single thread; plain statics.
        unsafe {
            (*addr_of_mut!(RESULTS))[row] = stats;
            RESULT_ROWS = row + 1;
        }
        let mut line = ui::Line::new();
        summary_line(&mut line, row, &stats);
        ui::live_line(font, (row as u32) % ui::LIVE_ROWS, line.as_str(), colour(&stats));
        ui::sub(row + 1, row_count());
        tty_row(row, &stats);
    }
    tty::println("hardware-tests: gpu-done complete");
}

fn tty_row(row: usize, st: &Stats) {
    let mut l = ui::Line::new();
    l.s("hardware-tests: gpu-done ").s(table()[row].name);
    l.s(" ok=").u(st.ok).s("/").u(REPEATS as u32);
    l.s(" istat=").u(st.istat).s(" istat_only=").u(st.istat_only);
    tty::println(l.as_str());
    let mut l = ui::Line::new();
    l.s("hardware-tests: gpu-done   med_polls=").u(st.med_polls);
    l.s(" med_us=").u(us(st.med_clocks));
    l.s(" tmo=").u(st.timeouts);
    l.s(" b26=").u(st.b26).s(" b27=").u(st.b27).s(" b28=").u(st.b28);
    l.s(" dma=").u(st.dma_busy).s(" unquiet=").u(st.unquiet);
    tty::println(l.as_str());
}

fn colour(st: &Stats) -> (u8, u8, u8) {
    if st.ok as usize == REPEATS {
        (150, 230, 160)
    } else if st.ok == 0 {
        (255, 110, 110)
    } else {
        (255, 200, 90)
    }
}

fn pad_to(line: &mut ui::Line, name: &str, width: usize) {
    line.s(name);
    for _ in name.len()..width {
        line.s(" ");
    }
}

fn pad_num(line: &mut ui::Line, value: u32, width: usize) {
    let mut digits = 1;
    let mut v = value;
    while v >= 10 {
        v /= 10;
        digits += 1;
    }
    for _ in digits..width {
        line.s(" ");
    }
    line.u(value);
}

fn summary_line(line: &mut ui::Line, row: usize, st: &Stats) {
    pad_to(line, table()[row].name, 12);
    pad_num(line, st.ok, 3);
    line.s("/").u(REPEATS as u32);
}

pub(crate) fn page_count() -> usize {
    // SAFETY: single thread; plain static.
    if unsafe { RESULT_ROWS } == 0 {
        0
    } else {
        3
    }
}

/// Draw page `page` (0 to 2), every frame.
pub(crate) fn draw_page(font: &FontAtlas, page: usize) {
    // SAFETY: single thread; plain statics.
    let (results, rows) = unsafe { (&*addr_of!(RESULTS), RESULT_ROWS) };
    let head = (255, 232, 128);
    let plain = (176, 190, 210);
    match page {
        0 => {
            font.draw_text_scaled_q8(4, 2, match unsafe { SET } { 0 => "GPU DONE SIGNAL", 1 => "GPU DONE FIX", _ => "GPU DONE FILLS" }, 512, 512, head);
            let mut y = 20;
            for (row, st) in results.iter().take(rows).enumerate() {
                let mut line = ui::Line::new();
                summary_line(&mut line, row, st);
                font.draw_text_scaled_q8(4, y, line.as_str(), 352, 352, colour(st));
                y += 11;
            }
        }
        1 => {
            font.draw_text(4, 4, "GPU DONE SIGNAL  STATS", head);
            font.draw_text(4, 14, "ROW          OK IRQ IRQ- POLLS     US", plain);
            let mut y = 25;
            for (row, st) in results.iter().take(rows).enumerate() {
                let mut line = ui::Line::new();
                pad_to(&mut line, table()[row].name, 12);
                line.s(" ");
                pad_num(&mut line, st.ok, 2);
                line.s(" ");
                pad_num(&mut line, st.istat, 3);
                line.s(" ");
                pad_num(&mut line, st.istat_only, 4);
                line.s(" ");
                pad_num(&mut line, st.med_polls, 5);
                line.s(" ");
                pad_num(&mut line, us(st.med_clocks), 6);
                font.draw_text(4, y, line.as_str(), colour(st));
                y += 10;
            }
            font.draw_text(4, y + 4, "OK: BIT 24 ROSE.  IRQ: I_STAT BIT 1", plain);
            font.draw_text(4, y + 14, "IRQ-: I_STAT ROSE, BIT 24 DID NOT.", plain);
            font.draw_text(4, y + 24, "POLLS, US: MEDIAN OVER THE OK ONES.", plain);
        }
        _ => {
            font.draw_text(4, 4, "STATE AT EACH TIMEOUT", head);
            font.draw_text(4, 14, "ROW          TMO B26 B27 B28 DMA UNQ", plain);
            let mut y = 25;
            for (row, st) in results.iter().take(rows).enumerate() {
                let mut line = ui::Line::new();
                pad_to(&mut line, table()[row].name, 12);
                line.s(" ");
                pad_num(&mut line, st.timeouts, 3);
                line.s(" ");
                pad_num(&mut line, st.b26, 3);
                line.s(" ");
                pad_num(&mut line, st.b27, 3);
                line.s(" ");
                pad_num(&mut line, st.b28, 3);
                line.s(" ");
                pad_num(&mut line, st.dma_busy, 3);
                line.s(" ");
                pad_num(&mut line, st.unquiet, 3);
                font.draw_text(4, y, line.as_str(), colour(st));
                y += 10;
            }
            font.draw_text(4, y + 4, "B26 B27 B28: GPU READY BITS SET AT", plain);
            font.draw_text(4, y + 14, "THE TIMEOUT. DMA: CHANNEL 2 STILL BUSY.", plain);
            font.draw_text(4, y + 24, "UNQ: GPU NOT QUIET BEFORE THE ATTEMPT.", plain);
        }
    }
    font.draw_text(4, 230, "LEFT/RIGHT PAGE   START MENU", plain);
}
