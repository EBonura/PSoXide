// SPDX-License-Identifier: GPL-2.0-or-later
//! GPU cost probes, submitted the way a game submits a frame.
//!
//! The fill battery in main.rs (records `0xA0`-`0xAF`) writes its primitives
//! straight to GP0 without pacing and takes GPUSTAT bit 26 as "done". The v1.22
//! console captures showed what that measures: 64 to 144 words pushed into a
//! 16-word FIFO while the GPU is busy drawing, and an end marker that pulses
//! high between primitives. Textured triangles read faster than flat ones and
//! a VRAM fill slower than a rectangle. Its textures were also sampled from
//! the framebuffer, mostly texel 0, which the GPU skips as transparent.
//!
//! Here each batch is a linked list of one packet per primitive, handed to DMA
//! channel 2, which feeds the GPU at the GPU's own pace. The list ends with a
//! GP0(1Fh) interrupt request: that command is taken in order with the
//! drawing, so GPUSTAT bit 24 going high means the last primitive has been
//! drawn. Textures are opaque.
//!
//! Timer 2 runs from the DMA kick to the interrupt flag. The poll loop costs
//! about a dozen cycles a turn, which is the resolution. Everything draws into
//! off-screen VRAM. Ids are `0x100` and up (the extended timing block).

use crate::{push_timing_record, sample_timing, TimingRecord, TIMING_RECORD_COUNT};
use psx_gpu::{Resolution, VideoMode};
use psx_io::dma;
use psx_io::gpu as gpu_io;
use psx_io::timers;

/// Where the batches draw: right of both framebuffers, below the font atlas.
const DRAW_X: u32 = 640;
const DRAW_Y: u32 = 400;
/// Probe textures: two copies of one 64x64 4bpp image in neighbouring texture
/// pages, so a record can alternate pages without changing what is sampled.
const TEX_X: u16 = 768;
const TEX_Y: u16 = 256;
const TEX_PAGE_A: u32 = (TEX_X as u32 / 64) | (1 << 4);
const TEX_PAGE_B: u32 = TEX_PAGE_A + 1;
/// Two 256-entry CLUT rows. The first 16 entries serve the 4bpp modes.
const CLUT_X: u16 = 768;
const CLUT_Y: u16 = 336;
const CLUT_A: u32 = ((CLUT_Y as u32) << 6) | (CLUT_X as u32 / 16);
const CLUT_B: u32 = ((CLUT_Y as u32 + 1) << 6) | (CLUT_X as u32 / 16);
const DEPTH_4BPP: u32 = 0;
const DEPTH_8BPP: u32 = 1 << 7;
const BLEND_AVERAGE: u32 = 0;

const MAX_PRIMS: usize = 64;
/// Header plus up to nine words a primitive, then the interrupt request.
static mut LIST: [u32; MAX_PRIMS * 10 + 2] = [0; MAX_PRIMS * 10 + 2];

#[derive(Copy, Clone, PartialEq, Eq)]
enum Shape {
    FlatTri,
    GouraudTri,
    /// `page`/`clut` alternate between the A and B copies when asked to.
    TexTri,
    GouraudTexTri,
    FlatRect,
    TexRect,
    VramFill,
    VramCopy,
}

#[derive(Copy, Clone)]
struct Case {
    id: u16,
    count: u16,
    size: u32,
    shape: Shape,
    /// OR-ed into the command byte: bit 0 raw texture, bit 1 semi-transparent.
    command_bits: u32,
    eight_bpp: bool,
    alternate_page: bool,
    alternate_clut: bool,
    /// Texel span across the primitive.
    span: u32,
    dither: bool,
    clipped: bool,
    letterboxed: bool,
}

const fn case(id: u16, count: u16, size: u32, shape: Shape) -> Case {
    Case {
        id,
        count,
        size,
        shape,
        command_bits: 0,
        eight_bpp: false,
        alternate_page: false,
        alternate_clut: false,
        span: 32,
        dither: false,
        clipped: false,
        letterboxed: false,
    }
}

impl Case {
    const fn bits(mut self, bits: u32) -> Self {
        self.command_bits = bits;
        self
    }
    const fn eight_bpp(mut self) -> Self {
        self.eight_bpp = true;
        self
    }
    const fn alternate_page(mut self) -> Self {
        self.alternate_page = true;
        self
    }
    const fn alternate_clut(mut self) -> Self {
        self.alternate_clut = true;
        self
    }
    const fn span(mut self, span: u32) -> Self {
        self.span = span;
        self
    }
    const fn dither(mut self) -> Self {
        self.dither = true;
        self
    }
    const fn clipped(mut self) -> Self {
        self.clipped = true;
        self
    }
    const fn letterboxed(mut self) -> Self {
        self.letterboxed = true;
        self
    }
}

const RAW: u32 = 1 << 24;
const TRANSLUCENT: u32 = 1 << 25;

const CASES: [Case; 21] = [
    // Sixteen 32x32 primitives each (512 pixels a triangle, 1024 a rect).
    case(0x100, 16, 32, Shape::FlatTri),
    case(0x101, 16, 32, Shape::GouraudTri),
    case(0x102, 16, 32, Shape::GouraudTri).dither(),
    case(0x103, 16, 32, Shape::TexTri),
    case(0x104, 16, 32, Shape::TexTri).bits(RAW),
    case(0x105, 16, 32, Shape::TexTri).bits(TRANSLUCENT),
    case(0x107, 16, 32, Shape::FlatTri).bits(TRANSLUCENT),
    case(0x108, 16, 32, Shape::FlatRect),
    case(0x109, 16, 32, Shape::TexRect),
    // Texture-cache and CLUT-cache behaviour, against 0x103 and 0x10A.
    case(0x10A, 16, 32, Shape::TexRect).eight_bpp(),
    case(0x10B, 16, 32, Shape::TexRect)
        .eight_bpp()
        .alternate_clut(),
    case(0x10C, 16, 32, Shape::TexTri).alternate_page(),
    case(0x10D, 16, 32, Shape::TexTri).span(63),
    // What the GPU does with work it cannot show, and with the picture off.
    case(0x10E, 16, 32, Shape::FlatTri).clipped(),
    case(0x10F, 16, 32, Shape::TexTri).letterboxed(),
    // Other ways to move a 32x32 block, against 0x108.
    case(0x110, 16, 32, Shape::VramFill),
    case(0x111, 16, 32, Shape::VramCopy),
    // Two-pixel triangles: nothing to fill, so what is left is setup.
    case(0x112, 64, 2, Shape::FlatTri),
    case(0x113, 64, 2, Shape::TexTri).span(2),
    case(0x114, 64, 2, Shape::GouraudTexTri).span(2),
    // Last on purpose: it leaves a lit, textured triangle in off-screen VRAM,
    // so a VRAM dump shows at a glance that packets, texture and CLUT are sane.
    case(0x106, 16, 32, Shape::GouraudTexTri),
];

pub(crate) fn push(records: &mut [TimingRecord; TIMING_RECORD_COUNT], next: &mut usize) {
    upload_textures();
    for entry in CASES {
        let record = sample_timing(entry.id, entry.count, || run(&entry));
        push_timing_record(records, next, record);
    }
}

/// A 64x64 image with no zero texel in either depth, and two CLUT rows with
/// no zero colour: texel or colour 0x0000 is transparent, and the GPU skips
/// transparent pixels faster than it draws opaque ones.
fn upload_textures() {
    // 4bpp packs four texels a word; 0x4321-style nibbles are never zero, and
    // as 8bpp pairs (0x21, 0x43, ...) they are never zero either.
    let mut texture = [0u16; 16 * 64];
    for (index, word) in texture.iter_mut().enumerate() {
        let n = (index as u16 % 13) + 1;
        *word = n | ((n % 15 + 1) << 4) | (((n + 5) % 15 + 1) << 8) | (((n + 9) % 15 + 1) << 12);
    }
    for page in 0..2u16 {
        psx_vram::upload_16bpp(
            psx_vram::VramRect::new(TEX_X + page * 64, TEX_Y, 16, 64),
            &texture,
        );
    }
    let mut clut = [0u16; 256];
    for row in 0..2u16 {
        for (index, colour) in clut.iter_mut().enumerate() {
            *colour = 0x0421 * ((index as u16 + row * 7) % 31 + 1);
        }
        psx_vram::upload_16bpp(psx_vram::VramRect::new(CLUT_X, CLUT_Y + row, 256, 1), &clut);
    }
}

fn run(entry: &Case) -> u16 {
    let head = build_list(entry);
    set_environment(entry);
    if entry.letterboxed {
        // nocash: the GPU renders at full speed only while it is not also
        // fetching the picture, and a one-line display range removes nearly
        // all of that. The screen blanks for the length of the batch.
        gpu_io::write_gp1(0x0700_0000 | 0x10 | (0x11 << 10));
    }
    let elapsed = submit_and_time(head);
    if entry.letterboxed {
        psx_gpu::set_screen_v_offset(0, VideoMode::Ntsc, Resolution::R320X240);
    }
    elapsed
}

fn set_environment(entry: &Case) {
    gpu_io::wait_cmd_ready();
    if entry.clipped {
        // A drawing area nowhere near the primitives: all of them are
        // received and rejected, the price of leaving culling to the GPU.
        gpu_io::write_gp0(0xE300_0000 | 960 | (DRAW_Y << 10));
        gpu_io::write_gp0(0xE400_0000 | 975 | ((DRAW_Y + 15) << 10));
    } else {
        gpu_io::write_gp0(0xE300_0000);
        gpu_io::write_gp0(0xE400_0000 | 1023 | (511 << 10));
    }
    gpu_io::write_gp0(0xE500_0000);
    // Rects take their page from the draw mode; polygons carry their own.
    let depth = if entry.eight_bpp {
        DEPTH_8BPP
    } else {
        DEPTH_4BPP
    };
    let dither = if entry.dither { 1 << 9 } else { 0 };
    gpu_io::write_gp0(0xE100_0000 | TEX_PAGE_A | depth | BLEND_AVERAGE | dither);
    gpu_io::write_gp0(0xE200_0000);
    gpu_io::write_gp0(0xE600_0000);
    gpu_io::wait_cmd_ready();
}

fn build_list(entry: &Case) -> u32 {
    let list = (&raw mut LIST) as *mut u32;
    let mut at = 0usize;
    let mut emit = |words: &[u32], last: bool| {
        // SAFETY: the list is sized for MAX_PRIMS of the largest packet.
        unsafe {
            let next = if last {
                0x00FF_FFFF
            } else {
                list.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF
            };
            core::ptr::write_volatile(list.add(at), ((words.len() as u32) << 24) | next);
            for (offset, word) in words.iter().enumerate() {
                core::ptr::write_volatile(list.add(at + 1 + offset), *word);
            }
        }
        at += 1 + words.len();
    };

    let depth = if entry.eight_bpp {
        DEPTH_8BPP
    } else {
        DEPTH_4BPP
    };
    let size = entry.size;
    let span = entry.span;
    for index in 0..u32::from(entry.count) {
        let x = DRAW_X;
        let y = DRAW_Y + (index & 7);
        let (x1, y1) = (x + size, y + size);
        let page = if entry.alternate_page && index & 1 == 1 {
            TEX_PAGE_B
        } else {
            TEX_PAGE_A
        };
        let clut = if entry.alternate_clut && index & 1 == 1 {
            CLUT_B
        } else {
            CLUT_A
        };
        let xy = |px: u32, py: u32| (py << 16) | px;
        let bits = entry.command_bits;
        match entry.shape {
            Shape::FlatTri => emit(&[0x2000_80FF | bits, xy(x, y), xy(x1, y), xy(x, y1)], false),
            Shape::GouraudTri => emit(
                &[
                    0x3000_00FF | bits,
                    xy(x, y),
                    0x0000_FF00,
                    xy(x1, y),
                    0x00FF_0000,
                    xy(x, y1),
                ],
                false,
            ),
            Shape::TexTri => emit(
                &[
                    0x2480_8080 | bits,
                    xy(x, y),
                    clut << 16,
                    xy(x1, y),
                    ((page | depth) << 16) | span,
                    xy(x, y1),
                    span << 8,
                ],
                false,
            ),
            Shape::GouraudTexTri => emit(
                &[
                    0x3480_8080 | bits,
                    xy(x, y),
                    clut << 16,
                    0x0080_FF80,
                    xy(x1, y),
                    ((page | depth) << 16) | span,
                    0x00FF_8080,
                    xy(x, y1),
                    span << 8,
                ],
                false,
            ),
            Shape::FlatRect => emit(&[0x6000_80FF | bits, xy(x, y), xy(size, size)], false),
            Shape::TexRect => emit(
                &[0x6480_8080 | bits, xy(x, y), clut << 16, xy(size, size)],
                false,
            ),
            Shape::VramFill => emit(&[0x0200_80FF, xy(x, y), xy(size, size)], false),
            Shape::VramCopy => emit(
                &[0x8000_0000, xy(x, DRAW_Y + 64), xy(x, y), xy(size, size)],
                false,
            ),
        }
    }
    // The end marker: an interrupt request, taken in order with the drawing.
    emit(&[0x1F00_0000], true);
    list as u32
}

fn submit_and_time(head: u32) -> u16 {
    let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
    gpu_io::write_gp1(0x0200_0000); // acknowledge any stale GPU interrupt
    gpu_io::write_gp1(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    dma::set_madr(dma::Channel::Gpu, head);
    dma::set_bcr_manual(dma::Channel::Gpu, 0);
    // GP1 writes take a few cycles to reach GPUSTAT, and an acknowledge that
    // is still in flight when the list's interrupt request arrives cancels
    // it. Let both settle before the clock starts.
    for _ in 0..64 {
        let _ = gpu_io::gpustat();
    }

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    dma::set_chcr(
        dma::Channel::Gpu,
        dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_LINKED | dma::CHCR_START,
    );
    let mut polls = 0u32;
    while gpu_io::gpustat().bits() & (1 << 24) == 0 && polls < 1_000_000 {
        polls += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);

    gpu_io::write_gp1(0x0200_0000);
    gpu_io::write_gp1(0x0400_0000 | old_direction);
    if polls == 1_000_000 {
        0xFFFF
    } else {
        elapsed
    }
}
