//! `hello-i64probe` -- does software 64-bit integer arithmetic execute
//! correctly on the PS1 (via compiler-builtins' `__muldi3` / `__divdi3` etc.)?
//!
//! Each operand is run through `core::hint::black_box` so nothing const-folds on
//! the host: the multiply / divide compile to real runtime calls the target CPU
//! must execute. A green `ALL PASS` means i64 works on hardware/emulator.
//!
//! This probe uncovered a real bug: compiler-builtins' **signed** 64-bit divide
//! (`__divdi3`) returns garbage on this target, while unsigned mul/div/mod work.
//! `psx-rt` now overrides `__divdi3`/`__moddi3` with a correct implementation, so
//! the `signed / and %` case here passes purely from linking psx-rt.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

struct Case {
    label: &'static str,
    got: i64,
    want: i64,
}

/// Fixed-point multiply a*b/ONE done in UNSIGNED magnitude (avoids the broken
/// signed __divdi3), sign handled by hand.
fn fmul64(a: i64, b: i64, one: u64) -> i64 {
    let neg = (a < 0) ^ (b < 0);
    let m = (a.unsigned_abs() * b.unsigned_abs()) / one; // u64 mul + u64 div
    if neg {
        -(m as i64)
    } else {
        m as i64
    }
}
/// Fixed-point divide a*ONE/b in unsigned magnitude.
fn fdiv64(a: i64, b: i64, one: u64) -> i64 {
    let neg = (a < 0) ^ (b < 0);
    let m = (a.unsigned_abs() * one) / b.unsigned_abs();
    if neg {
        -(m as i64)
    } else {
        m as i64
    }
}

fn cases(out: &mut [Case; 8]) -> usize {
    let one = black_box(1_000_000u64);
    // Unsigned 64-bit multiply / divide / mod (these should be the good path).
    out[0] = Case {
        label: "u64 mul",
        got: (black_box(300_000u64) * black_box(150_000u64)) as i64,
        want: 45_000_000_000,
    };
    out[1] = Case {
        label: "u64 div",
        got: (black_box(45_000_000_000u64) / black_box(1000u64)) as i64,
        want: 45_000_000,
    };
    out[2] = Case {
        label: "u64 mod",
        got: (black_box(45_000_000_007u64) % black_box(1000u64)) as i64,
        want: 7,
    };
    // The workaround the spreadsheet would use: signed via unsigned magnitude.
    out[3] = Case {
        label: "fmul64 1.5*2",
        got: fmul64(black_box(1_500_000), black_box(2_000_000), one),
        want: 3_000_000,
    };
    out[4] = Case {
        label: "fdiv64 10/3",
        got: fdiv64(black_box(10_000_000), black_box(3_000_000), one),
        want: 3_333_333,
    };
    out[5] = Case {
        label: "fdiv64 -10/3",
        got: fdiv64(black_box(-10_000_000), black_box(3_000_000), one),
        want: -3_333_333,
    };
    out[6] = Case {
        label: "big u64 div",
        got: (black_box(9_876_543_210_000u64) / black_box(1_000_000u64)) as i64,
        want: 9_876_543,
    };
    // Direct SIGNED i64 divide + mod. These returned garbage from the stock
    // compiler-builtins; psx-rt now overrides __divdi3/__moddi3, so they pass.
    out[7] = Case {
        label: "signed / and %",
        got: black_box(-123_456_789i64) / black_box(1000i64)
            + black_box(-7i64) % black_box(1000i64),
        want: -123_463, // -123456 + -7
    };
    8
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    let mut buf = [Case {
        label: "",
        got: 0,
        want: 0,
    }; 8];
    let n = cases(&mut buf);
    let all_ok = buf[..n].iter().all(|c| c.got == c.want);

    loop {
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "I64 PROBE (black_box)", (220, 220, 230));
        let mut y: i16 = 26;
        for c in buf[..n].iter() {
            let ok = c.got == c.want;
            let tint = if ok { GREEN } else { RED };
            font.draw_text(8, y, if ok { "OK" } else { "XX" }, tint);
            font.draw_text(32, y, c.label, tint);
            let mut nb = [0u8; 24];
            let s = fmt_i64(c.got, &mut nb);
            font.draw_text(180, y, s, tint);
            y += 12;
        }
        let (banner, bt) = if all_ok {
            ("ALL PASS", GREEN)
        } else {
            ("FAIL", RED)
        };
        font.draw_text(8, y + 8, banner, bt);

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}

impl Copy for Case {}
impl Clone for Case {
    fn clone(&self) -> Self {
        *self
    }
}

/// Format an i64 as decimal into `out`, returning the `&str`.
fn fmt_i64(v: i64, out: &mut [u8]) -> &str {
    let neg = v < 0;
    let mut mag = v.unsigned_abs();
    let mut tmp = [0u8; 20];
    let mut i = 0;
    if mag == 0 {
        tmp[0] = b'0';
        i = 1;
    }
    while mag > 0 {
        tmp[i] = b'0' + (mag % 10) as u8;
        mag /= 10;
        i += 1;
    }
    let mut n = 0;
    if neg {
        out[n] = b'-';
        n += 1;
    }
    for j in (0..i).rev() {
        out[n] = tmp[j];
        n += 1;
    }
    // SAFETY: only ASCII digits and '-'.
    unsafe { core::str::from_utf8_unchecked(&out[..n]) }
}
