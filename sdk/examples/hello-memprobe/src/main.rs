//! `hello-memprobe` -- do `psx-rt`'s hand-scheduled `memcpy` / `memset` /
//! `memcmp` behave exactly like the C definitions on the target?
//!
//! Every size and alignment combination up to a few words past the block
//! loop, plus a few long copies, is run through the real symbols (sizes go
//! through `core::hint::black_box` so LLVM emits calls instead of inline
//! expansions) and checked bytewise against a reference loop, guard bytes
//! included. The verdict is printed on the TTY (`MEMPROBE PASS` /
//! `MEMPROBE FAIL <count>`) and drawn on screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

extern "C" {
    fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
    fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8;
    fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32;
}

const BUF: usize = 1200;
const GUARD: u8 = 0xEE;

static mut SRC: [u8; BUF] = [0; BUF];
static mut DST: [u8; BUF] = [0; BUF];
/// First failures as (kind, a, b, n, bad): kind 1 memcpy, 2 memset, 3 memcmp.
static mut LOG: [[u32; 5]; 16] = [[0; 5]; 16];
static mut LOGGED: usize = 0;

fn log_failure(kind: u32, a: usize, b: usize, n: usize, bad: u32) {
    unsafe {
        if LOGGED < 16 {
            LOG[LOGGED] = [kind, a as u32, b as u32, n as u32, bad];
            LOGGED += 1;
        }
    }
}

fn fill_pattern(buf: &mut [u8], seed: u8) {
    let mut v = seed;
    for b in buf.iter_mut() {
        *b = v;
        v = v.wrapping_mul(31).wrapping_add(7);
    }
}

/// One memcpy of `n` bytes from `src + so` to `dst + do_`; returns the count
/// of wrong bytes (copied region and one guard word on each side).
fn check_memcpy(so: usize, do_: usize, n: usize) -> u32 {
    let (src, dst) = unsafe { (&mut *core::ptr::addr_of_mut!(SRC), &mut *core::ptr::addr_of_mut!(DST)) };
    fill_pattern(src, (so + n) as u8);
    for b in dst.iter_mut() {
        *b = GUARD;
    }
    let ret = unsafe { memcpy(dst.as_mut_ptr().add(do_), src.as_ptr().add(so), black_box(n)) };
    let mut bad = 0u32;
    if ret != unsafe { dst.as_mut_ptr().add(do_) } {
        bad += 1;
    }
    for i in 0..n {
        if dst[do_ + i] != src[so + i] {
            bad += 1;
        }
    }
    for i in do_.saturating_sub(4)..do_ {
        if dst[i] != GUARD {
            bad += 1;
        }
    }
    for i in (do_ + n)..(do_ + n + 4).min(BUF) {
        if dst[i] != GUARD {
            bad += 1;
        }
    }
    bad
}

fn check_memset(do_: usize, n: usize, value: u8) -> u32 {
    let dst = unsafe { &mut *core::ptr::addr_of_mut!(DST) };
    for b in dst.iter_mut() {
        *b = GUARD;
    }
    // The high bits of `c` must be ignored, as in C.
    let c = (value as i32) | 0x1234_5600;
    let ret = unsafe { memset(dst.as_mut_ptr().add(do_), black_box(c), black_box(n)) };
    let mut bad = 0u32;
    if ret != unsafe { dst.as_mut_ptr().add(do_) } {
        bad += 1;
    }
    for i in 0..n {
        if dst[do_ + i] != value {
            bad += 1;
        }
    }
    for i in do_.saturating_sub(4)..do_ {
        if dst[i] != GUARD {
            bad += 1;
        }
    }
    for i in (do_ + n)..(do_ + n + 4).min(BUF) {
        if dst[i] != GUARD {
            bad += 1;
        }
    }
    bad
}

fn reference_memcmp(a: &[u8], b: &[u8]) -> i32 {
    for i in 0..a.len() {
        if a[i] != b[i] {
            return a[i] as i32 - b[i] as i32;
        }
    }
    0
}

/// Compare `n` bytes at offsets `ao`/`bo`; `differ_at` (if < n) flips one
/// byte of `b` so the sign and position handling is exercised.
fn check_memcmp(ao: usize, bo: usize, n: usize, differ_at: usize, up: bool) -> u32 {
    let (a, b) = unsafe { (&mut *core::ptr::addr_of_mut!(SRC), &mut *core::ptr::addr_of_mut!(DST)) };
    fill_pattern(a, (n + differ_at) as u8);
    b.copy_from_slice(a);
    // Shift b so the two views hold the same bytes at different alignments.
    for i in 0..n {
        b[bo + i] = a[ao + i];
    }
    if differ_at < n {
        b[bo + differ_at] = if up { a[ao + differ_at].wrapping_add(1) } else { a[ao + differ_at].wrapping_sub(1) };
        // Keep the reference honest when the wrap changed the sign.
    }
    let got = unsafe { memcmp(a.as_ptr().add(ao), b.as_ptr().add(bo), black_box(n)) };
    let want = reference_memcmp(&a[ao..ao + n], &b[bo..bo + n]);
    let same_sign = (got == 0 && want == 0) || (got < 0 && want < 0) || (got > 0 && want > 0);
    if same_sign {
        0
    } else {
        let bytes = ((a[ao] as usize) << 24) | ((b[bo] as usize) << 16) | ((a[ao + n - 1] as usize) << 8) | (b[bo + n - 1] as usize);
        log_failure(4, got as u32 as usize, want as u32 as usize, bytes, (differ_at as u32) << 1 | u32::from(up));
        1
    }
}

fn run_all() -> (u32, u32) {
    let mut cases = 0u32;
    let mut failures = 0u32;
    let sizes: [usize; 22] = [0, 1, 2, 3, 4, 5, 7, 8, 12, 15, 16, 17, 20, 31, 32, 33, 47, 48, 63, 64, 100, 257];
    for &n in sizes.iter() {
        for so in 0..4 {
            for do_ in 0..4 {
                cases += 1;
                let bad = check_memcpy(so + 8, do_ + 8, n);
                if bad != 0 {
                    failures += 1;
                    log_failure(1, so + 8, do_ + 8, n, bad);
                }
            }
        }
        for do_ in 0..4 {
            for value in [0x5Au8, 0x00] {
                cases += 1;
                let bad = check_memset(do_ + 8, n, value);
                if bad != 0 {
                    failures += 1;
                    log_failure(2, value as usize, do_ + 8, n, bad);
                }
            }
        }
        for ao in 0..4 {
            for bo in 0..4 {
                let positions = [n, 0, n / 2, n.saturating_sub(1), 4, 5, 9, 17];
                for &p in positions.iter() {
                    if p > n {
                        continue;
                    }
                    for up in [true, false] {
                        cases += 1;
                        let bad = check_memcmp(ao + 8, bo + 8, n, p, up);
                        if bad != 0 {
                            failures += 1;
                            log_failure(3, ao + 8, bo + 8, n, (p as u32) << 1 | u32::from(up));
                        }
                    }
                }
            }
        }
    }
    // Long copies past the block loop with every alignment pair.
    for &n in [1000usize, 1023, 1024, 1100].iter() {
        for so in 0..4 {
            for do_ in 0..4 {
                cases += 1;
                let bad = check_memcpy(so + 8, do_ + 8, n);
                if bad != 0 {
                    failures += 1;
                    log_failure(1, so + 8, do_ + 8, n, bad);
                }
            }
        }
        cases += 1;
        let bad = check_memset(9, n, 0xA5);
        if bad != 0 {
            failures += 1;
            log_failure(2, 0xA5, 9, n, bad);
        }
    }
    (cases, failures)
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    let (cases, failures) = run_all();
    if failures == 0 {
        tty::print("MEMPROBE PASS cases=");
        tty::print_hex_u32(cases);
        tty::println("");
    } else {
        tty::print("MEMPROBE FAIL ");
        tty::print_hex_u32(failures);
        tty::print(" of ");
        tty::print_hex_u32(cases);
        tty::println("");
        let logged = unsafe { LOGGED };
        for entry in unsafe { LOG[..logged].iter() } {
            tty::print("  kind/a/b/n/bad ");
            for value in entry.iter() {
                tty::print_hex_u32(*value);
                tty::print(" ");
            }
            tty::println("");
        }
    }

    loop {
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "MEM PROBE (memcpy/memset/memcmp)", (220, 220, 230));
        let (banner, tint) = if failures == 0 { ("ALL PASS", GREEN) } else { ("FAIL", RED) };
        font.draw_text(8, 30, banner, tint);
        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}
