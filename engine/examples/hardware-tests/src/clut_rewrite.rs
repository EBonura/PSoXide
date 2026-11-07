// SPDX-License-Identifier: GPL-2.0-or-later
//! Direct palette-cache observations. No stale/fresh result is presumed.
//! Two raw textured draws use red, then the same palette address is uploaded
//! blue. Nothing reads VRAM or draws text between those draws. Both pixels
//! and the rewritten palette are read only after the final ordered drain.

use crate::console_tests::record;
use crate::v24::{self, xy};
use crate::{bounds, push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::gpu as gpu_io;

/// rec clut_rewrite: first_pixel, second_pixel, descriptor_flags (ids 0xA00 to 0xA18; descriptor bits 0 to 2 variant: 0 same word queued, 1 same word drained, 2 GP0 01h then same word, 3 changed word, 4 changed word then return; bits 3 to 5 sample: 0 4bpp rectangle index 1, 1 8bpp rectangle index 1, 2 8bpp rectangle index 17, 3 4bpp triangle index 1, 4 8bpp triangle index 17; flags bits 8 drain timeout, 9 read timeout, 10 mixed adjacent pixels, 11 palette readback differs from blue, 12 first pixel differs from red, 13 packet-ready timeout)
pub(crate) const FIRST: u16 = 0xA00;
const RED: u32 = 0x001F;
const BLUE: u32 = 0x7C00;
const CLUT_Y: u32 = 504;
const TEX_X: u32 = 768;
const TEX_Y: u32 = 256;
const DRAIN_TIMEOUT: u32 = 1 << 8;
const READ_TIMEOUT: u32 = 1 << 9;
const MIXED_PIXELS: u32 = 1 << 10;
const BAD_PALETTE: u32 = 1 << 11;
const BAD_FIRST: u32 = 1 << 12;
const READY_TIMEOUT: u32 = 1 << 13;

fn packet(words: &[u32], flags: &mut u32) {
    let mut left = bounds::scale(200_000);
    while gpu_io::status().bits() & (1 << 26) == 0 {
        if left == 0 {
            *flags |= READY_TIMEOUT;
            return;
        }
        left -= 1;
    }
    v24::send(words);
}

fn upload(x: u32, y: u32, width: u32, colour: u32, flags: &mut u32) {
    packet(&[0xA000_0000, xy(x, y), xy(width, 1)], flags);
    if *flags & READY_TIMEOUT == 0 {
        for _ in 0..width / 2 {
            gpu_io::write_command(colour | (colour << 16));
        }
    }
}

fn drain(flags: &mut u32) {
    if !v24::drain() {
        *flags |= DRAIN_TIMEOUT;
    }
}

fn sample(x: u32, y: u32, flags: &mut u32) -> u32 {
    packet(&[0xC000_0000, xy(x, y), xy(2, 1)], flags);
    let mut left = bounds::scale(200_000);
    while gpu_io::status().bits() & v24::STAT_VRAM_READY == 0 {
        if left == 0 {
            *flags |= READ_TIMEOUT;
            return 0xFFFF;
        }
        left -= 1;
    }
    let word = gpu_io::read_data();
    if word as u16 != (word >> 16) as u16 {
        *flags |= MIXED_PIXELS;
    }
    word & 0xFFFF
}

fn draw(group: u32, x: u32, y: u32, clut: u32, flags: &mut u32) {
    let eight = matches!(group, 1 | 2 | 4);
    let page = (TEX_X / 64) | (1 << 4) | ((eight as u32) << 7);
    if group >= 3 {
        // Raw opaque triangle, all three UVs sample the same opaque index.
        packet(
            &[
                0x2580_8080,
                xy(x, y),
                clut << 16,
                xy(x + 4, y),
                page << 16,
                xy(x, y + 4),
                0,
            ],
            flags,
        );
    } else {
        packet(&[0xE100_0000 | page | (1 << 10)], flags);
        packet(&[0x6580_8080, xy(x, y), clut << 16, xy(4, 4)], flags);
    }
}

pub(crate) fn run(records: &mut [TimingRecord; TIMING_RECORD_COUNT], next: &mut usize) {
    for group in 0..5 {
        for variant in 0..5 {
            let id = FIRST + (group * 5 + variant) as u16;
            #[cfg(not(feature = "clut-preview"))]
            bounds::record_start(id);
            let mut flags = variant | (group << 3);
            let eight = matches!(group, 1 | 2 | 4);
            let width = if eight { 256 } else { 16 };
            let index = if matches!(group, 2 | 4) { 17 } else { 1 };
            let texture = if eight { index | (index << 8) } else { 0x1111 };
            let clut_a = CLUT_Y << 6;
            let clut_b = (CLUT_Y + 1) << 6;
            let x = 640 + group * 16;
            let y = 400 + variant * 8;
            drain(&mut flags);
            packet(
                &[
                    0xE100_0400,
                    0xE200_0000,
                    0xE300_0000,
                    0xE400_0000 | 1023 | (511 << 10),
                    0xE500_0000,
                    0xE600_0000,
                ],
                &mut flags,
            );
            upload(TEX_X, TEX_Y, 2, texture, &mut flags);
            upload(0, CLUT_Y, width, RED, &mut flags);
            upload(0, CLUT_Y + 1, width, BLUE, &mut flags);
            // Upload clears the destination to distinguish missing draws.
            upload(x, y, 16, 0, &mut flags);
            for row in 1..4 {
                upload(x, y + row, 16, 0, &mut flags);
            }
            packet(&[0x0100_0000], &mut flags);
            drain(&mut flags);
            draw(group, x, y, clut_a, &mut flags);
            if variant == 1 {
                drain(&mut flags);
            }
            upload(0, CLUT_Y, width, BLUE, &mut flags);
            if variant == 1 {
                drain(&mut flags);
            }
            if variant == 2 {
                packet(&[0x0100_0000], &mut flags);
            }
            if variant == 4 {
                // This draw changes the relevant cache-line address before
                // returning to the rewritten original address.
                draw(group, x + 12, y, clut_b, &mut flags);
            }
            let second_clut = if variant == 3 { clut_b } else { clut_a };
            draw(group, x + 8, y, second_clut, &mut flags);
            drain(&mut flags);
            let first = sample(x + 1, y + 1, &mut flags);
            let second = sample(x + 9, y + 1, &mut flags);
            if sample(index, CLUT_Y, &mut flags) != BLUE {
                flags |= BAD_PALETTE;
            }
            if first != RED {
                flags |= BAD_FIRST;
            }
            push_timing_record(records, next, record(id, first, second, flags));
            if flags & (DRAIN_TIMEOUT | READ_TIMEOUT | READY_TIMEOUT) != 0 {
                v24::reset_command_buffer();
            }
        }
    }
    let mut flags = 0;
    packet(&[0xE100_0000], &mut flags);
    drain(&mut flags);
}

/// Short headless renderer witness. It submits the exact measurement rows,
/// then copies their offscreen pixels into the displayed framebuffer.
#[cfg(feature = "clut-preview")]
pub(crate) fn preview() -> ! {
    use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
    probe_gpu!(gpu);
    gpu.set_display(DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240));
    gpu.set_display_start((0, 0));
    gpu.set_display_enabled(true);
    let mut records = [TimingRecord::pending(); TIMING_RECORD_COUNT];
    let mut next = 0;
    run(&mut records, &mut next);
    let mut flags = 0;
    upload(0, 0, 320, 0, &mut flags);
    for row in 1..240 {
        upload(0, row, 320, 0, &mut flags);
    }
    packet(
        &[0x8000_0000, xy(640, 400), xy(120, 96), xy(80, 40)],
        &mut flags,
    );
    drain(&mut flags);
    for row in &records[..next] {
        psx_rt::tty::print("clut-preview: ");
        psx_rt::tty::print(crate::report::hex4(row.id).as_str());
        psx_rt::tty::print(" first=");
        crate::report::tty_print_dec_u16(row.min);
        psx_rt::tty::print(" second=");
        crate::report::tty_print_dec_u16(row.med);
        psx_rt::tty::print(" flags=");
        crate::report::tty_print_dec_u16(row.max);
        psx_rt::tty::println("");
    }
    psx_rt::tty::println("clut-preview: complete");
    loop {
        core::hint::spin_loop();
    }
}
