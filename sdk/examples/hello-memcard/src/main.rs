//! `hello-memcard` -- a `psx-mc` smoke test that runs a full memory-card
//! round-trip on boot and paints the result, so it can be verified headlessly
//! (dump the frame and read the PASS/FAIL banner).
//!
//! Steps, each shown as a line: format the card, write an uncompressed save,
//! read it back and compare, then the same for a compressed save. A green
//! `ALL PASS` banner means the SIO0 transport + filesystem + compression all
//! work end-to-end against the attached card.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_mc::{Card, Error, HardwareCard, Slot};
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const NAME: &str = "BASLUS-00001MCTEST";

const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);
const GREY: (u8, u8, u8) = (160, 160, 170);

/// Up to this many result lines are recorded, then rendered every frame.
struct Report {
    lines: [Line; 12],
    len: usize,
    all_ok: bool,
}

#[derive(Copy, Clone)]
struct Line {
    text: [u8; 40],
    len: u8,
    ok: bool,
    info: bool, // informational (grey), not a pass/fail
}

impl Report {
    fn new() -> Self {
        Report {
            lines: [Line {
                text: [0; 40],
                len: 0,
                ok: true,
                info: false,
            }; 12],
            len: 0,
            all_ok: true,
        }
    }

    fn push(&mut self, label: &str, ok: bool, info: bool) {
        if self.len >= self.lines.len() {
            return;
        }
        let mut l = Line {
            text: [0; 40],
            len: 0,
            ok,
            info,
        };
        let b = label.as_bytes();
        let n = b.len().min(40);
        l.text[..n].copy_from_slice(&b[..n]);
        l.len = n as u8;
        self.lines[self.len] = l;
        self.len += 1;
        if !ok && !info {
            self.all_ok = false;
        }
    }

    fn step(&mut self, label: &str, r: Result<(), Error>) -> bool {
        match r {
            Ok(()) => {
                self.push(label, true, false);
                true
            }
            Err(e) => {
                // Append the error code so failures are diagnosable on screen.
                let mut buf = [0u8; 40];
                let ln = fmt_err(label, e, &mut buf);
                self.push(str_of(&buf[..ln]), false, false);
                false
            }
        }
    }
}

/// Run the whole test and return the filled report.
fn run() -> Report {
    let mut rep = Report::new();
    let mut card = Card::new(HardwareCard::new(Slot::One));

    // Presence + format state (informational).
    match card.is_formatted() {
        Ok(true) => rep.push("CARD: formatted", true, true),
        Ok(false) => rep.push("CARD: blank", true, true),
        Err(e) => {
            let mut b = [0u8; 40];
            let n = fmt_err("CARD", e, &mut b);
            rep.push(str_of(&b[..n]), false, false);
            return rep;
        }
    }

    if !rep.step("FORMAT", card.format()) {
        return rep;
    }

    // Uncompressed round-trip.
    let payload = sample_payload();
    if !rep.step("WRITE", card.write(NAME, "PSX-MC TEST", &payload)) {
        return rep;
    }
    let mut buf = [0u8; 512];
    match card.read(NAME, &mut buf) {
        Ok(n) if n == payload.len() && buf[..n] == payload => {
            rep.push("READ+VERIFY", true, false)
        }
        Ok(_) => rep.push("READ+VERIFY mismatch", false, false),
        Err(e) => {
            let mut b = [0u8; 40];
            let ln = fmt_err("READ", e, &mut b);
            rep.push(str_of(&b[..ln]), false, false);
        }
    }

    // Compressed round-trip on a sparse payload.
    let mut sparse = [0u8; 400];
    sparse[0] = b'S';
    sparse[1] = b'C';
    for k in 0..6 {
        sparse[64 * k + 3] = k as u8;
    }
    let mut scratch = [0u8; 512];
    if rep.step(
        "WRITE-C",
        card.write_compressed(NAME, "PSX-MC TEST", &sparse, &mut scratch),
    ) {
        let mut cbuf = [0u8; 512];
        match card.read(NAME, &mut cbuf) {
            Ok(n) if n == sparse.len() && cbuf[..n] == sparse => {
                rep.push("READ-C+VERIFY", true, false)
            }
            Ok(_) => rep.push("READ-C mismatch", false, false),
            Err(e) => {
                let mut b = [0u8; 40];
                let ln = fmt_err("READ-C", e, &mut b);
                rep.push(str_of(&b[..ln]), false, false);
            }
        }
    }

    rep
}

fn sample_payload() -> [u8; 64] {
    let mut p = [0u8; 64];
    let msg = b"psx-mc round-trip ";
    p[..msg.len()].copy_from_slice(msg);
    for i in msg.len()..64 {
        p[i] = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    p
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    // Run once at boot; hold the result on screen.
    let rep = run();

    loop {
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "PSX-MC SMOKE TEST", (220, 220, 230));
        let mut y: i16 = 26;
        for i in 0..rep.len {
            let l = rep.lines[i];
            let tint = if l.info {
                GREY
            } else if l.ok {
                GREEN
            } else {
                RED
            };
            let mark: &str = if l.info {
                "--"
            } else if l.ok {
                "OK"
            } else {
                "XX"
            };
            font.draw_text(8, y, mark, tint);
            font.draw_text(32, y, str_of(&l.text[..l.len as usize]), tint);
            y += 12;
        }
        let (banner, btint) = if rep.all_ok {
            ("ALL PASS", GREEN)
        } else {
            ("FAIL", RED)
        };
        font.draw_text(8, 210, banner, btint);

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}

fn fmt_err(label: &str, e: Error, out: &mut [u8; 40]) -> usize {
    let code: &str = match e {
        Error::NoCard => "no-card",
        Error::Protocol => "protocol",
        Error::BadChecksum => "checksum",
        Error::OutOfRange => "range",
        Error::NotFormatted => "unformatted",
        Error::NotFound => "not-found",
        Error::NoSpace => "no-space",
        Error::Exists => "exists",
        Error::Corrupt => "corrupt",
        Error::BufferTooSmall => "buf-small",
        Error::BadContainer => "container",
        Error::Compression => "compress",
        Error::BadName => "bad-name",
    };
    let mut n = 0;
    for &b in label.as_bytes() {
        if n < 40 {
            out[n] = b;
            n += 1;
        }
    }
    for &b in b" ".iter().chain(code.as_bytes()) {
        if n < 40 {
            out[n] = b;
            n += 1;
        }
    }
    n
}

fn str_of(b: &[u8]) -> &str {
    // SAFETY: all rendered bytes are ASCII (labels + hex + error codes).
    unsafe { core::str::from_utf8_unchecked(b) }
}
