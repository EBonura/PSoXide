// SPDX-License-Identifier: GPL-2.0-or-later
//! How long GPU DMA channel 2 stays busy on a linked list whose nodes draw
//! (cases `0xD3`-`0xE9`, v1.24).
//!
//! The emulator's default model clears CHCR bit 24 after a word-count formula
//! (`gpu_command_linked_cycles` in bus.rs: words + words/16 + 9 a node +
//! nodes/5 + 5) that ignores what the nodes draw. Its experimental FIFO model
//! (`PSOXIDE_EXPERIMENTAL_DMA_FIFO=1`) admits payload only as the GPU drains
//! its FIFO, so a list of expensive primitives keeps the channel busy for
//! about as long as the drawing. An hl-psx optimisation measures +21% under
//! the first model and nothing under the second, so the console decides which
//! one to keep.
//!
//! Four lists, each ending in GP0(1Fh):
//!
//! * empty: 16 empty nodes, the control;
//! * cheap: 16 nodes of one tiny Gouraud triangle each;
//! * expensive: the same 16 nodes and packets, but each triangle covers half
//!   of a 320x240 area;
//! * packed: the expensive triangles four to a node, so every node carries
//!   24 words, more than the GPU's 16-word FIFO.
//!
//! Cheap and expensive differ only in vertex coordinates: same words, same
//! nodes. The default model therefore gives them the same busy time to the
//! cycle; the FIFO model gives the expensive list roughly its draw time.
//!
//! After the kick one poll loop stamps, on Timer 2 at the system clock
//! extended to 32 bits in software on every poll (so no list can overflow
//! it): CHCR bit 24 clearing, GPUSTAT bit 24 (the list's GP0(1Fh)) rising,
//! and the start of the final high run of GPUSTAT bits 28 and 26. Then the
//! CPU's throughput while channel 2 walks the expensive list: a fixed
//! I-cache-resident loop of eight ALU ops, eight RAM loads or eight
//! scratchpad loads, counted until CHCR clears, against the same loop idle.

use core::ptr::{addr_of, addr_of_mut, write_volatile};

use psx_io::gpu as gpu_io;
use psx_io::{dma, irq, timers};

use crate::payload::fnv32_words;
use crate::{gpu_read_word_at, IrqGuard, TestResult};

const TRIANGLES: usize = 16;
/// Triangles a node in the packed list: 24 GP0 words, over the 16-word FIFO.
const PACKED_PER_NODE: usize = 4;
const GOURAUD_WORDS: usize = 6;
/// Header plus packet for every triangle, then the GP0(1Fh) node.
const LIST_WORDS: usize = TRIANGLES * (GOURAUD_WORDS + 1) + 2;
static mut LIST: [u32; LIST_WORDS] = [0; LIST_WORDS];

const KICK: u32 = dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_LINKED | dma::CHCR_START;
/// A stamp that never happened.
pub(crate) const NOT_SEEN: u32 = u32::MAX;
/// Give up on a list after this many clocks (about half a second).
const POLL_LIMIT: u32 = 16_000_000;
const GPUSTAT_IRQ: u32 = 1 << 24;
const GPUSTAT_CMD_READY: u32 = 1 << 26;
const GPUSTAT_DMA_READY: u32 = 1 << 28;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Empty,
    Cheap,
    Expensive,
    Packed,
}

const KINDS: [Kind; 4] = [Kind::Empty, Kind::Cheap, Kind::Expensive, Kind::Packed];

/// One Gouraud triangle. `full` covers half of the 320x240 draw area, split
/// along the diagonal alternately so consecutive triangles cover all of it;
/// otherwise a two-pixel triangle. Every word's top byte is 0x00 or 0x30, so
/// a packet the GPU loses sync inside can only ever read as a NOP, a cache
/// clear or another triangle, all clipped to the draw area.
fn triangle(index: usize, full: bool) -> [u32; GOURAUD_WORDS] {
    let xy = |x: u32, y: u32| (y << 16) | x;
    let shade = ((index as u32 * 12) & 0x7F) + 0x10;
    let (a, b, c) = if full {
        if index & 1 == 0 {
            (xy(0, 0), xy(319, 0), xy(0, 239))
        } else {
            (xy(319, 239), xy(0, 239), xy(319, 0))
        }
    } else {
        let x = 16 + (index as u32 % 8) * 4;
        let y = 16 + (index as u32 / 8) * 4;
        (xy(x, y), xy(x + 2, y), xy(x, y + 2))
    };
    [0x3000_0000 | shade, a, shade << 8, b, shade << 16, c]
}

/// Build `kind`'s list. Returns the head and `(words << 16) | nodes`, headers
/// included, which is what the default model's formula takes.
fn build_list(kind: Kind) -> (u32, u32) {
    let list = addr_of_mut!(LIST) as *mut u32;
    let mut at = 0usize;
    let mut nodes = 0u32;
    let mut emit = |words: &[u32], last: bool| {
        let next = if last {
            0x00FF_FFFF
        } else {
            unsafe { list.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF }
        };
        // SAFETY: LIST is sized for one header per triangle plus the end node.
        unsafe {
            write_volatile(list.add(at), ((words.len() as u32) << 24) | next);
            for (offset, word) in words.iter().enumerate() {
                write_volatile(list.add(at + 1 + offset), *word);
            }
        }
        at += 1 + words.len();
        nodes += 1;
    };
    match kind {
        Kind::Empty => {
            for _ in 0..TRIANGLES {
                emit(&[], false);
            }
        }
        Kind::Cheap | Kind::Expensive => {
            for index in 0..TRIANGLES {
                emit(&triangle(index, kind == Kind::Expensive), false);
            }
        }
        Kind::Packed => {
            for node in 0..TRIANGLES / PACKED_PER_NODE {
                let mut words = [0u32; PACKED_PER_NODE * GOURAUD_WORDS];
                for k in 0..PACKED_PER_NODE {
                    words[k * GOURAUD_WORDS..(k + 1) * GOURAUD_WORDS]
                        .copy_from_slice(&triangle(node * PACKED_PER_NODE + k, true));
                }
                emit(&words, false);
            }
        }
    }
    emit(&[0x1F00_0000], true);
    (list as u32, ((at as u32) << 16) | nodes)
}

/// GP1 writes take a few cycles to reach GPUSTAT, and an acknowledge still in
/// flight when the list's interrupt request arrives cancels it (gpu_probes).
fn settle() {
    for _ in 0..64 {
        let _ = gpu_io::gpustat();
    }
}

/// Draw environment for the 320x240 area at (0, 0), a black fill of it, and
/// GP0(1Fh) as a fence so the fill is finished before any clock starts.
fn set_environment() {
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xE100_0400); // no dither, drawing to the display area allowed
    gpu_io::write_gp0(0xE200_0000);
    gpu_io::write_gp0(0xE300_0000);
    gpu_io::write_gp0(0xE400_0000 | 319 | (239 << 10));
    gpu_io::write_gp0(0xE500_0000);
    gpu_io::write_gp0(0xE600_0000);
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0x0200_0000);
    gpu_io::write_gp0(0);
    gpu_io::write_gp0((240 << 16) | 320);
    gpu_io::write_gp1(0x0200_0000);
    settle();
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0x1F00_0000);
    let mut polls = 0u32;
    while gpu_io::gpustat().bits() & GPUSTAT_IRQ == 0 && polls < 1_000_000 {
        polls += 1;
    }
    gpu_io::write_gp1(0x0200_0000);
    irq::ack(1 << irq::source::GPU);
}

/// A list built, the area cleared, channel 2 armed but not started.
struct Armed {
    shape: u32,
    old_direction: u32,
}

fn arm(kind: Kind) -> Armed {
    let (head, shape) = build_list(kind);
    set_environment();
    let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
    gpu_io::write_gp1(0x0200_0000);
    gpu_io::write_gp1(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    dma::set_madr(dma::Channel::Gpu, head);
    dma::set_bcr_manual(dma::Channel::Gpu, 0);
    settle();
    Armed {
        shape,
        old_direction,
    }
}

/// Put the GPU and channel 2 back whatever the list did.
fn disarm(armed: &Armed, irq_seen: bool) {
    if dma::is_busy(dma::Channel::Gpu) {
        dma::abort(dma::Channel::Gpu);
        gpu_io::write_gp1(0x0100_0000);
    } else if !irq_seen {
        // The GPU lost its place in the list: drop whatever it is waiting on.
        gpu_io::write_gp1(0x0100_0000);
    }
    gpu_io::write_gp1(0x0200_0000);
    irq::ack(1 << irq::source::GPU);
    gpu_io::write_gp1(0x0400_0000 | armed.old_direction);
}

/// Timer 2 at the system clock, widened to 32 bits by counting wraps. Every
/// caller reads it far more often than once per 65,536 clocks.
struct Clock {
    base: u32,
    high: u32,
    last: u16,
}

impl Clock {
    fn start() -> Self {
        timers::set_mode(timers::Timer::Timer2, 0);
        let now = timers::counter(timers::Timer::Timer2);
        Self {
            base: now as u32,
            high: 0,
            last: now,
        }
    }

    #[inline(always)]
    fn now(&mut self) -> u32 {
        let counter = timers::counter(timers::Timer::Timer2);
        if counter < self.last {
            self.high = self.high.wrapping_add(0x1_0000);
        }
        self.last = counter;
        self.high
            .wrapping_add(counter as u32)
            .wrapping_sub(self.base)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Stamps {
    /// `(words << 16) | nodes`, headers included.
    pub(crate) shape: u32,
    /// Clocks from the kick to CHCR bit 24 clear.
    pub(crate) chcr: u32,
    /// ... to GPUSTAT bit 24, the list's GP0(1Fh).
    pub(crate) irq: u32,
    /// ... to the start of GPUSTAT bit 28's final high run.
    pub(crate) bit28: u32,
    /// ... to the start of GPUSTAT bit 26's final high run.
    pub(crate) bit26: u32,
}

/// Kick an armed list and stamp its four events. Interrupts are masked for
/// the walk, at most `POLL_LIMIT` clocks.
fn kick_and_stamp(armed: &Armed) -> Stamps {
    let mut s = Stamps {
        shape: armed.shape,
        chcr: NOT_SEEN,
        irq: NOT_SEEN,
        bit28: NOT_SEEN,
        bit26: NOT_SEEN,
    };
    let guard = IrqGuard::mask();
    let mut clock = Clock::start();
    dma::set_chcr(dma::Channel::Gpu, KICK);
    loop {
        let t = clock.now();
        let busy = dma::is_busy(dma::Channel::Gpu);
        let stat = gpu_io::gpustat().bits();
        if !busy && s.chcr == NOT_SEEN {
            s.chcr = t;
        }
        if stat & GPUSTAT_IRQ != 0 && s.irq == NOT_SEEN {
            s.irq = t;
        }
        if stat & GPUSTAT_DMA_READY == 0 {
            s.bit28 = NOT_SEEN;
        } else if s.bit28 == NOT_SEEN {
            s.bit28 = t;
        }
        if stat & GPUSTAT_CMD_READY == 0 {
            s.bit26 = NOT_SEEN;
        } else if s.bit26 == NOT_SEEN {
            s.bit26 = t;
        }
        let ready = GPUSTAT_DMA_READY | GPUSTAT_CMD_READY;
        if !busy && s.irq != NOT_SEEN && stat & ready == ready {
            break;
        }
        if t > POLL_LIMIT {
            break;
        }
    }
    drop(guard);
    disarm(armed, s.irq != NOT_SEEN);
    s
}

/// Warm-up run, then the measured one.
fn stamp(kind: Kind) -> Stamps {
    let _ = kick_and_stamp(&arm(kind));
    kick_and_stamp(&arm(kind))
}

/// Twelve pixel pairs spread over the draw area, hashed.
fn sample_pixels() -> u32 {
    let mut words = [0u32; 12];
    let mut index = 0;
    for y in [8u16, 120, 230] {
        for x in [8u16, 96, 200, 300] {
            words[index] = gpu_read_word_at(x, y);
            index += 1;
        }
    }
    fnv32_words(&words)
}

// ---------------------------------------------------------------------------
// CPU throughput during the walk
// ---------------------------------------------------------------------------

/// Idle iterations the loop's cost is measured over.
const IDLE_ITERATIONS: u32 = 256;
/// A walk that has not ended after this many iterations never will.
const WALK_CAP: u32 = 1 << 20;

static mut RAM_DATA: [u32; 16] = [0; 16];
const SCRATCHPAD: u32 = 0x1F80_0000;

/// `$body` then one read of channel 2's CHCR and one of Timer 2, per
/// iteration, until `(CHCR ^ mask)` bit 24 is clear or `cap` iterations have
/// run. With `mask` = bit 24 and an idle channel the CHCR test never passes,
/// so the idle measurement runs exactly the instructions the walk does.
/// Timer 2 must be on the system clock; its wraps are counted every
/// iteration, on the low halfword only (a word read of the counter returns
/// junk above it). Returns (iterations, clocks). `$24` is the load address.
macro_rules! walk_loop {
    ($name:ident, $body:literal) => {
        #[inline(never)]
        fn $name(mask: u32, cap: u32, data: u32) -> (u32, u32) {
            let iterations: u32;
            let wraps: u32;
            let first: u32;
            let last: u32;
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    ".balign 16",
                    "lui $8, 0x1F80",
                    "ori $8, $8, 0x10A8",
                    "lui $11, 0x1F80",
                    "ori $11, $11, 0x1120",
                    "move $12, $zero",
                    "move $17, $zero",
                    "lw $16, 0($11)",
                    "nop",
                    "andi $16, $16, 0xFFFF",
                    "move $18, $16",
                    "1:",
                    $body,
                    "lw $10, 0($8)",
                    "lw $14, 0($11)",
                    "xor $10, $10, $15",
                    "andi $14, $14, 0xFFFF",
                    "sltu $25, $14, $16",
                    "addu $17, $17, $25",
                    "move $16, $14",
                    "srl $10, $10, 24",
                    "andi $10, $10, 1",
                    "beqz $10, 2f",
                    "addiu $12, $12, 1",
                    "bne $12, $13, 1b",
                    "nop",
                    "2:",
                    ".set reorder",
                    in("$15") mask,
                    in("$13") cap,
                    in("$24") data,
                    lateout("$8") _,
                    lateout("$9") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") iterations,
                    lateout("$14") _,
                    lateout("$16") last,
                    lateout("$17") wraps,
                    lateout("$18") first,
                    lateout("$25") _,
                    options(nostack)
                );
            }
            (
                iterations,
                (wraps << 16).wrapping_add(last).wrapping_sub(first),
            )
        }
    };
}

walk_loop!(walk_alu, ".rept 8\naddiu $9, $9, 1\n.endr");
walk_loop!(walk_loads, ".rept 8\nlw $9, 0($24)\n.endr");

#[derive(Clone, Copy)]
enum LoopKind {
    Alu,
    Ram,
    Scratchpad,
}

fn run_loop(kind: LoopKind, mask: u32, cap: u32) -> (u32, u32) {
    match kind {
        LoopKind::Alu => walk_alu(mask, cap, 0),
        LoopKind::Ram => walk_loads(mask, cap, addr_of!(RAM_DATA) as u32),
        LoopKind::Scratchpad => walk_loads(mask, cap, SCRATCHPAD),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Throughput {
    /// Clocks for `IDLE_ITERATIONS` iterations with channel 2 idle.
    pub(crate) idle: u32,
    /// Iterations completed while channel 2 walked the expensive list.
    pub(crate) iterations: u32,
    /// Clocks those iterations took, from just after the kick to CHCR clear.
    pub(crate) cycles: u32,
}

fn throughput(kind: LoopKind) -> Throughput {
    let guard = IrqGuard::mask();
    timers::set_mode(timers::Timer::Timer2, 0);
    let _ = run_loop(kind, dma::CHCR_START, IDLE_ITERATIONS);
    let (_, idle) = run_loop(kind, dma::CHCR_START, IDLE_ITERATIONS);
    drop(guard);

    let armed = arm(Kind::Expensive);
    let guard = IrqGuard::mask();
    timers::set_mode(timers::Timer::Timer2, 0);
    dma::set_chcr(dma::Channel::Gpu, KICK);
    let (iterations, cycles) = run_loop(kind, 0, WALK_CAP);
    // Let the drawing finish before anything else touches the GPU.
    let mut clock = Clock::start();
    while gpu_io::gpustat().bits() & GPUSTAT_IRQ == 0 && clock.now() < POLL_LIMIT {}
    let irq_seen = gpu_io::gpustat().bits() & GPUSTAT_IRQ != 0;
    drop(guard);
    disarm(&armed, irq_seen);
    Throughput {
        idle,
        iterations,
        cycles,
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Results {
    lists: [Stamps; 4],
    pixels_unpacked: u32,
    pixels_packed: u32,
    loops: [Throughput; 3],
}

fn measure() -> Results {
    let mut lists = [Stamps {
        shape: 0,
        chcr: NOT_SEEN,
        irq: NOT_SEEN,
        bit28: NOT_SEEN,
        bit26: NOT_SEEN,
    }; 4];
    let mut pixels_unpacked = 0;
    let mut pixels_packed = 0;
    for (slot, kind) in lists.iter_mut().zip(KINDS) {
        *slot = stamp(kind);
        match kind {
            Kind::Expensive => pixels_unpacked = sample_pixels(),
            Kind::Packed => pixels_packed = sample_pixels(),
            _ => {}
        }
    }
    let loops = [
        throughput(LoopKind::Alu),
        throughput(LoopKind::Ram),
        throughput(LoopKind::Scratchpad),
    ];
    psx_gpu::set_draw_area(0, 0, 1023, 511);
    psx_gpu::set_draw_offset(0, 0);
    Results {
        lists,
        pixels_unpacked,
        pixels_packed,
        loops,
    }
}

static mut RESULTS: Option<Results> = None;

/// The first case measures everything; the rest read its result, or measure
/// if RESUME FROM TEST started past the first.
fn results(first: bool) -> Results {
    let slot = unsafe { &mut *addr_of_mut!(RESULTS) };
    if first || slot.is_none() {
        *slot = Some(measure());
    }
    slot.unwrap()
}

macro_rules! stamp_case {
    ($name:ident, $list:literal, $field:ident, $first:literal, $note:literal) => {
        pub(crate) fn $name() -> TestResult {
            let s = results($first).lists[$list];
            TestResult::info(s.shape, s.$field, $note)
        }
    };
}

stamp_case!(test_empty_chcr, 0, chcr, true, "chcr clear");
stamp_case!(test_empty_irq, 0, irq, false, "gp0 1f irq");
stamp_case!(test_empty_bit28, 0, bit28, false, "bit28 high");
stamp_case!(test_empty_bit26, 0, bit26, false, "bit26 high");
stamp_case!(test_cheap_chcr, 1, chcr, false, "chcr clear");
stamp_case!(test_cheap_irq, 1, irq, false, "gp0 1f irq");
stamp_case!(test_cheap_bit28, 1, bit28, false, "bit28 high");
stamp_case!(test_cheap_bit26, 1, bit26, false, "bit26 high");
stamp_case!(test_expensive_chcr, 2, chcr, false, "chcr clear");
stamp_case!(test_expensive_irq, 2, irq, false, "gp0 1f irq");
stamp_case!(test_expensive_bit28, 2, bit28, false, "bit28 high");
stamp_case!(test_expensive_bit26, 2, bit26, false, "bit26 high");
stamp_case!(test_packed_chcr, 3, chcr, false, "chcr clear");
stamp_case!(test_packed_irq, 3, irq, false, "gp0 1f irq");
stamp_case!(test_packed_bit28, 3, bit28, false, "bit28 high");
stamp_case!(test_packed_bit26, 3, bit26, false, "bit26 high");

/// `0xE3`: 24-word nodes must draw what 6-word nodes draw. A mismatch means
/// the GPU lost words from a node larger than its FIFO, and the packed
/// list's timings describe some other workload.
pub(crate) fn test_packed_pixels() -> TestResult {
    let r = results(false);
    if r.pixels_packed == r.pixels_unpacked && r.lists[3].irq != NOT_SEEN {
        TestResult::pass(r.pixels_unpacked, r.pixels_packed, "same pixels")
    } else {
        TestResult::fail(r.pixels_unpacked, r.pixels_packed, "packed differs")
    }
}

macro_rules! loop_cases {
    ($counts:ident, $cycles:ident, $index:literal, $note:literal) => {
        /// Iterations during the walk (bits 0-15), idle clocks for 256
        /// iterations (16-31), each saturating.
        pub(crate) fn $counts() -> TestResult {
            let t = results(false).loops[$index];
            TestResult::info(
                IDLE_ITERATIONS,
                t.iterations.min(0xFFFF) | (t.idle.min(0xFFFF) << 16),
                $note,
            )
        }

        /// Clocks from the kick to CHCR clear, as the loop saw them.
        pub(crate) fn $cycles() -> TestResult {
            let t = results(false).loops[$index];
            TestResult::info(t.iterations, t.cycles, "walk clocks")
        }
    };
}

loop_cases!(test_alu_counts, test_alu_cycles, 0, "alu iters");
loop_cases!(test_ram_counts, test_ram_cycles, 1, "ram iters");
loop_cases!(test_spad_counts, test_spad_cycles, 2, "spad iters");
