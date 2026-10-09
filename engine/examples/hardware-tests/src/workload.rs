// SPDX-License-Identifier: GPL-2.0-or-later
//! Whole-workload timing and the texture UV-window rows, v2.4.
//!
//! **Calibration scene** (`0x8C0`-`0x8C5`). The per-primitive rows say what a
//! packet costs; a game is a few hundred of them with the CPU working in
//! between. This is one fixed scene of 256 textured triangles in a linked
//! list (about 300 pixels each, positions and texture windows from a fixed
//! generator), and a fixed block of CPU work (a mixed ALU and RAM-load loop),
//! timed three ways on the system clock extended to 32 bits: the list alone,
//! the CPU work alone, and the CPU work started right behind the list's DMA
//! kick, so the DMA's bus use shows in the CPU's time and the GPU's tail in the
//! wait. Then two frame loops, locked to VBlank, that count how many frames the
//! same work takes: one list and one block of work a frame, and four of each.
//! Every number is the median of three runs.
//!
//! **UV windows** (`0x8E0`-`0x93F`). Sixteen 32 x 32 textured triangles per
//! row, as in the texture-state rows `0x200`-`0x223`, but with the texture
//! coordinates swept over a whole 256 x 256 texture page instead of an 8 x 8
//! window: spans of 8 to 255 texels (magnified to minified) at each depth,
//! origins moved across the page, and the GP0(E2h) texture window shrunk to 8,
//! 16, 32 and 64 texels. Each row is a pair of records, the clocks for the
//! list and then a record that says what the row was.
//!

// The record constants document the layout; the code builds ids from their bases.
#![allow(dead_code)]

use crate::console_tests::{record, spread};
use crate::v24::{self, xy, Clock32};
use crate::{push_timing_record, sample_timing, TimingRecord, TIMING_RECORD_COUNT};
use core::hint::black_box;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use psx_io::dma;
use psx_io::gpu as gpu_io;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

pub(crate) const CALIBRATION_RECORD: u16 = 0x8C0;
/// rec calibration_gpu: cycles_low_half, cycles_high_half, triangles (the scene's list alone, kick to the GP0(1Fh) interrupt; median of three; 0x8C0)
const CALIBRATION_GPU_RECORD: u16 = 0x8C0;
/// rec calibration_cpu: cycles_low_half, cycles_high_half, iterations_over_16 (the CPU block alone; 0x8C1)
const CALIBRATION_CPU_RECORD: u16 = 0x8C1;
/// rec calibration_both: cycles_low_half, cycles_high_half, flags (the list kicked, the CPU block run, then the wait for the list; bit 0 a wait ran out; 0x8C2)
const CALIBRATION_BOTH_RECORD: u16 = 0x8C2;
/// rec calibration_both_split: cpu_done_low_half, cpu_done_high_half, wait_after_cpu_low_half (clocks from the kick to the CPU block finishing, and from there to the list finishing; 0x8C3)
const CALIBRATION_BOTH_SPLIT_RECORD: u16 = 0x8C3;
/// rec calibration_frames_one: frames_taken, cycles_low_half, cycles_high_half (eight VBlank-locked frames of one list and one CPU block; 0x8C4)
const CALIBRATION_FRAMES_ONE_RECORD: u16 = 0x8C4;
/// rec calibration_frames_four: frames_taken, cycles_low_half, cycles_high_half (four VBlank-locked frames of four lists with a CPU block after each kick; 0x8C5)
const CALIBRATION_FRAMES_FOUR_RECORD: u16 = 0x8C5;
/// rec uv_window: clocks_min, clocks_med, clocks_max (clocks for sixteen 32 x 32 textured triangles, five runs; the even id of each pair from 0x8E0; the odd id says which row it was)
const UV_WINDOW_TIMING_RECORD: u16 = 0x8E0;
/// rec uv_window_row: depth_and_window, uv_span_texels, origin_u_and_v (the odd id of each pair; depth 0 4bpp, 1 8bpp, 2 15bpp in bits 0 to 3, the texture window in 8-texel units in bits 4 to 8, 0 for off; origin u in the low byte and v in the high byte; 0x8E1 and every second id after)
const UV_WINDOW_ROW_RECORD: u16 = 0x8E1;
pub(crate) const UV_WINDOW_RECORD: u16 = 0x8E0;

const TRIANGLES: usize = 256;
const NODE_WORDS: usize = 8;
static mut SCENE: [u32; TRIANGLES * NODE_WORDS + 2] = [0; TRIANGLES * NODE_WORDS + 2];
const UV_TRIANGLES: usize = 16;
static mut UV_LIST: [u32; UV_TRIANGLES * NODE_WORDS + 2] = [0; UV_TRIANGLES * NODE_WORDS + 2];

/// Where the scene draws: a 320 x 240 area right of the picture, above the
/// texture pages.
const SCENE_X: u32 = 640;
const SCENE_Y: u32 = 0;
/// The scene's texture: a 4bpp 64 x 64 image and its CLUT.
const SCENE_TEX_X: u32 = 576;
const SCENE_TEX_Y: u32 = 256;
const SCENE_PAGE: u32 = (SCENE_TEX_X / 64) | (1 << 4);
const SCENE_CLUT: (u32, u32) = (576, 336);

const KICK: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;

static mut TABLE: [u32; 1024] = [0; 1024];
const CPU_ITERATIONS: u32 = 3_400;
const POLL_EVERY: u32 = 128;

fn lcg(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 8
}

/// A 64 x 64 4bpp image with no zero nibble, and a CLUT with no zero colour.
fn upload_scene_texture() {
    let mut row = [0u16; 16];
    for y in 0..64u32 {
        for (x, hw) in row.iter_mut().enumerate() {
            let n = |k: u32| ((x as u32 * 3 + y * 5 + k * 7) % 15 + 1) as u16;
            *hw = n(0) | (n(1) << 4) | (n(2) << 8) | (n(3) << 12);
        }
        psx_vram::upload_16bpp(
            psx_vram::VramRect::new(SCENE_TEX_X as u16, (SCENE_TEX_Y + y) as u16, 16, 1),
            &row,
        );
    }
    let mut clut = [0u16; 16];
    for (i, colour) in clut.iter_mut().enumerate() {
        *colour = 0x0421 * (i as u16 + 1);
    }
    psx_vram::upload_16bpp(
        psx_vram::VramRect::new(SCENE_CLUT.0 as u16, SCENE_CLUT.1 as u16, 16, 1),
        &clut,
    );
}

fn emit_node(list: *mut u32, at: &mut usize, words: &[u32], last: bool) {
    // SAFETY: the callers size their list for every node they emit.
    unsafe {
        let next = if last {
            0x00FF_FFFF
        } else {
            list.add(*at + 1 + words.len()) as u32 & 0x00FF_FFFF
        };
        write_volatile(list.add(*at), ((words.len() as u32) << 24) | next);
        for (offset, word) in words.iter().enumerate() {
            write_volatile(list.add(*at + 1 + offset), *word);
        }
    }
    *at += 1 + words.len();
}

fn build_scene() -> u32 {
    let list = addr_of_mut!(SCENE) as *mut u32;
    let mut at = 0usize;
    let mut state = 0x1234_5678u32;
    let clut = (SCENE_CLUT.1 << 6) | (SCENE_CLUT.0 / 16);
    for _ in 0..TRIANGLES {
        let x = SCENE_X + lcg(&mut state) % 280;
        let y = SCENE_Y + lcg(&mut state) % 200;
        let w = 12 + lcg(&mut state) % 28;
        let h = 12 + lcg(&mut state) % 28;
        let u = lcg(&mut state) % 24;
        let v = lcg(&mut state) % 24;
        emit_node(
            list,
            &mut at,
            &[
                0x2480_8080,
                xy(x, y),
                (clut << 16) | (v << 8) | u,
                xy(x + w, y),
                (SCENE_PAGE << 16) | (v << 8) | (u + w.min(39)),
                xy(x, y + h),
                ((v + h.min(39)) << 8) | u,
            ],
            false,
        );
    }
    emit_node(list, &mut at, &[0x1F00_0000], true);
    list as u32
}

/// The CPU's share: a mixed block of multiplies, RAM loads and stores, with
/// the clock read every `POLL_EVERY` iterations so a wrap cannot be lost.
fn cpu_block(clock: &mut Clock32) -> u32 {
    let table = addr_of_mut!(TABLE) as *mut u32;
    let mut acc = 0x9E37_79B9u32;
    for i in 0..CPU_ITERATIONS {
        acc = acc.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        // SAFETY: the index is masked to the table.
        unsafe {
            let slot = table.add(((acc >> 8) & 1023) as usize);
            let value = read_volatile(slot);
            write_volatile(slot, value.wrapping_add(acc));
            acc ^= read_volatile(table.add(((acc >> 3) & 1023) as usize));
        }
        if i % POLL_EVERY == 0 {
            let _ = clock.now();
        }
    }
    acc
}

fn kick(head: u32) {
    gpu_io::write_display_control(0x0200_0000);
    gpu_io::write_display_control(0x0400_0002);
    dma::enable_channel(dma::Channel::Gpu);
    // SAFETY: silicon probe: the transfer reads only the static list.
    unsafe {
        dma::raw::set_address(dma::Channel::Gpu, head);
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
    }
    for _ in 0..64 {
        let _ = gpu_io::status();
    }
}

/// Start the channel. BCR is written again just before, as every kick does.
fn go() {
    // SAFETY: as `kick`.
    unsafe {
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
        dma::raw::set_control(dma::Channel::Gpu, KICK);
    }
}

/// Wait for the list's GP0(1Fh) interrupt request; false when it never rose.
fn wait_marker(clock: &mut Clock32) -> bool {
    let mut polls = crate::bounds::scale(3_000_000);
    while gpu_io::status().bits() & v24::STAT_IRQ1 == 0 {
        if polls == 0 {
            return false;
        }
        polls -= 1;
        let _ = clock.now();
    }
    true
}

/// The next VBlank, with the clock read all the while so no wrap is lost.
fn wait_frame(clock: &mut Clock32) {
    let start = psx_rt::interrupts::vblank_count();
    let mut polls = crate::bounds::scale(4_000_000);
    while psx_rt::interrupts::vblank_count() == start && polls > 0 {
        polls -= 1;
        let _ = clock.now();
    }
}

fn finish_list() {
    gpu_io::write_display_control(0x0200_0000);
    gpu_io::write_display_control(0x0400_0000);
}

fn median3(values: &mut [u32; 3]) -> u32 {
    spread(values).1
}

/// The list alone: kick to marker.
fn gpu_only(head: u32) -> (u32, bool) {
    kick(head);
    let mut clock = Clock32::start();
    go();
    let ok = wait_marker(&mut clock);
    let t = clock.now();
    finish_list();
    (t, ok)
}

fn cpu_only() -> u32 {
    let mut clock = Clock32::start();
    black_box(cpu_block(&mut clock));
    clock.now()
}

/// The list started, then the CPU block, then the wait.
fn both(head: u32) -> (u32, u32, u32, bool) {
    kick(head);
    let mut clock = Clock32::start();
    go();
    black_box(cpu_block(&mut clock));
    let cpu_done = clock.now();
    let ok = wait_marker(&mut clock);
    let total = clock.now();
    finish_list();
    (total, cpu_done, total.saturating_sub(cpu_done), ok)
}

/// `iterations` VBlank-locked frames, each `lists` lists with a CPU block run
/// behind every kick. Returns the frames the VBlank counter advanced and the
/// clocks.
fn frame_loop(head: u32, iterations: u32, lists: u32) -> (u32, u32) {
    let mut clock = Clock32::start();
    wait_frame(&mut clock);
    let first = psx_rt::interrupts::vblank_count();
    let start = clock.now();
    for _ in 0..iterations {
        for _ in 0..lists {
            kick(head);
            go();
            black_box(cpu_block(&mut clock));
            let _ = wait_marker(&mut clock);
            finish_list();
        }
        wait_frame(&mut clock);
    }
    let frames = psx_rt::interrupts::vblank_count().wrapping_sub(first);
    (frames, clock.now().wrapping_sub(start))
}

fn calibration(records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(CALIBRATION_RECORD);
    upload_scene_texture();
    v24::environment(0);
    // Texture window and page for the polygons: E1 is for rects only.
    let head = build_scene();
    // SAFETY: single thread; the table is this module's.
    unsafe {
        for (i, slot) in (*addr_of_mut!(TABLE)).iter_mut().enumerate() {
            *slot = (i as u32).wrapping_mul(0x9E37_79B1);
        }
    }
    let mut gpu = [0u32; 3];
    let mut cpu = [0u32; 3];
    let mut both_t = [0u32; 3];
    let mut cpu_done = [0u32; 3];
    let mut after = [0u32; 3];
    let mut ran_out = 0u32;
    for slot in gpu.iter_mut() {
        let (t, ok) = gpu_only(head);
        *slot = t;
        ran_out |= (!ok) as u32;
    }
    for slot in cpu.iter_mut() {
        *slot = cpu_only();
    }
    for run in 0..3 {
        let (t, done, wait, ok) = both(head);
        both_t[run] = t;
        cpu_done[run] = done;
        after[run] = wait;
        ran_out |= (!ok) as u32;
    }
    let g = median3(&mut gpu);
    let c = median3(&mut cpu);
    let b = median3(&mut both_t);
    let d = median3(&mut cpu_done);
    let a = median3(&mut after);
    let mut push = |row: TimingRecord| push_timing_record(records, next, row);
    push(record(
        CALIBRATION_RECORD,
        g & 0xFFFF,
        g >> 16,
        TRIANGLES as u32,
    ));
    push(record(
        CALIBRATION_RECORD + 1,
        c & 0xFFFF,
        c >> 16,
        CPU_ITERATIONS / 16,
    ));
    push(record(CALIBRATION_RECORD + 2, b & 0xFFFF, b >> 16, ran_out));
    push(record(
        CALIBRATION_RECORD + 3,
        d & 0xFFFF,
        d >> 16,
        a & 0xFFFF,
    ));
    for (offset, iterations, lists) in [(4u16, 8u32, 1u32), (5, 4, 4)] {
        crate::bounds::record_start(CALIBRATION_RECORD + offset);
        let (frames, clocks) = frame_loop(head, iterations, lists);
        push(record(
            CALIBRATION_RECORD + offset,
            frames,
            clocks & 0xFFFF,
            clocks >> 16,
        ));
    }
}

// ----------------------------------------------------------- UV windows

const PAGE_15: (u32, u32) = (768, 256);
const PAGE_8: (u32, u32) = (512, 256);
const PAGE_4: (u32, u32) = (640, 256);
const UV_CLUT_4: (u32, u32) = (512, 200);
const UV_CLUT_8: (u32, u32) = (512, 202);
const UV_DRAW: (u32, u32) = (640, 100);

fn page_word(depth: u32) -> u32 {
    let (x, y) = match depth {
        0 => PAGE_4,
        1 => PAGE_8,
        _ => PAGE_15,
    };
    (x / 64) | ((y / 256) << 4) | (depth << 7)
}

/// A 256 x 256 texture page with no zero texel at each depth, and the CLUTs.
fn upload_uv_pages() {
    let mut row = [0u16; 256];
    for y in 0..256u32 {
        // 15bpp: 256 pixels a row.
        for (x, hw) in row.iter_mut().enumerate() {
            *hw = (1 + (x as u32 * 131 + y * 17) % 32_000) as u16;
        }
        psx_vram::upload_16bpp(
            psx_vram::VramRect::new(PAGE_15.0 as u16, (PAGE_15.1 + y) as u16, 256, 1),
            &row,
        );
        // 8bpp: 128 halfwords a row, two non-zero bytes each.
        for (x, hw) in row.iter_mut().take(128).enumerate() {
            let lo = 1 + (x as u32 * 3 + y) % 255;
            let hi = 1 + (x as u32 * 5 + y * 2) % 255;
            *hw = (lo | (hi << 8)) as u16;
        }
        psx_vram::upload_16bpp(
            psx_vram::VramRect::new(PAGE_8.0 as u16, (PAGE_8.1 + y) as u16, 128, 1),
            &row[..128],
        );
        // 4bpp: 64 halfwords a row, four non-zero nibbles each.
        for (x, hw) in row.iter_mut().take(64).enumerate() {
            let n = |k: u32| ((x as u32 * 3 + y * 5 + k * 7) % 15 + 1) as u16;
            *hw = n(0) | (n(1) << 4) | (n(2) << 8) | (n(3) << 12);
        }
        psx_vram::upload_16bpp(
            psx_vram::VramRect::new(PAGE_4.0 as u16, (PAGE_4.1 + y) as u16, 64, 1),
            &row[..64],
        );
    }
    let mut clut = [0u16; 256];
    for (i, colour) in clut.iter_mut().enumerate() {
        *colour = 0x0421 * (i as u16 % 31 + 1);
    }
    psx_vram::upload_16bpp(
        psx_vram::VramRect::new(UV_CLUT_4.0 as u16, UV_CLUT_4.1 as u16, 16, 1),
        &clut[..16],
    );
    psx_vram::upload_16bpp(
        psx_vram::VramRect::new(UV_CLUT_8.0 as u16, UV_CLUT_8.1 as u16, 256, 1),
        &clut,
    );
}

#[derive(Copy, Clone)]
struct Row {
    depth: u32,
    span: u32,
    origin_u: u32,
    origin_v: u32,
    /// Texture window size in 8-texel units; 0 for none.
    window: u32,
}

fn uv_list(row: &Row) -> u32 {
    let list = addr_of_mut!(UV_LIST) as *mut u32;
    let mut at = 0usize;
    let clut = match row.depth {
        0 => (UV_CLUT_4.1 << 6) | (UV_CLUT_4.0 / 16),
        1 => (UV_CLUT_8.1 << 6) | (UV_CLUT_8.0 / 16),
        _ => 0,
    };
    let page = page_word(row.depth);
    let (u0, v0, s) = (row.origin_u, row.origin_v, row.span);
    for index in 0..UV_TRIANGLES as u32 {
        let (x, y) = (UV_DRAW.0, UV_DRAW.1 + (index & 7));
        emit_node(
            list,
            &mut at,
            &[
                0x2480_8080,
                xy(x, y),
                (clut << 16) | (v0 << 8) | u0,
                xy(x + 32, y),
                (page << 16) | (v0 << 8) | (u0 + s),
                xy(x, y + 32),
                ((v0 + s) << 8) | u0,
            ],
            false,
        );
    }
    emit_node(list, &mut at, &[0x1F00_0000], true);
    list as u32
}

fn time_row(row: &Row) -> u16 {
    let head = uv_list(row);
    gpu_io::wait_command_ready();
    let mask = if row.window == 0 {
        0
    } else {
        // Window of `window * 8` texels: the bits above it come from the offset.
        let m = (256 / 8) - row.window;
        m | (m << 5)
    };
    v24::send(&[0xE200_0000 | mask, 0xE600_0000]);
    gpu_io::wait_command_ready();
    crate::gpu_probes::submit_and_time(head)
}

fn uv_rows() -> ([Row; 40], usize) {
    let mut rows = [Row {
        depth: 0,
        span: 32,
        origin_u: 0,
        origin_v: 0,
        window: 0,
    }; 40];
    let mut n = 0;
    for depth in 0..3u32 {
        for span in [8u32, 16, 32, 64, 128, 255] {
            rows[n] = Row {
                depth,
                span,
                origin_u: 0,
                origin_v: 0,
                window: 0,
            };
            n += 1;
        }
    }
    for depth in 0..3u32 {
        for (u, v) in [(8u32, 0u32), (0, 8), (100, 100), (223, 223)] {
            rows[n] = Row {
                depth,
                span: 32,
                origin_u: u,
                origin_v: v,
                window: 0,
            };
            n += 1;
        }
    }
    for depth in [0u32, 2] {
        for window in [1u32, 2, 4, 8] {
            rows[n] = Row {
                depth,
                span: 63,
                origin_u: 0,
                origin_v: 0,
                window,
            };
            n += 1;
        }
    }
    (rows, n)
}

fn uv_windows(records: &mut Records, next: &mut usize) {
    upload_uv_pages();
    v24::environment(0);
    let (rows, count) = uv_rows();
    let mut id = UV_WINDOW_RECORD;
    for row in &rows[..count] {
        let timing = sample_timing(id, UV_TRIANGLES as u16, || time_row(row));
        push_timing_record(records, next, timing);
        push_timing_record(
            records,
            next,
            record(
                id + 1,
                row.depth | (row.window << 4),
                row.span,
                row.origin_u | (row.origin_v << 8),
            ),
        );
        id += 2;
    }
    v24::environment(0);
}

pub(crate) fn run_calibration(records: &mut Records, next: &mut usize) {
    calibration(records, next);
}

pub(crate) fn run_uv(records: &mut Records, next: &mut usize) {
    uv_windows(records, next);
}
