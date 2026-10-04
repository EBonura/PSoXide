//! `hello-pack` -- a `psx_pack::cd` smoke test that streams WORLD.PAK chunks
//! off the disc on boot and paints the result, so it can be verified
//! headlessly (dump the frame and read the PASS/FAIL banner, or grep the TTY
//! for `hello-pack: ALL PASS`).
//!
//! The disc is built by `make hello-pack-disc`: an 86-chunk fixture pack
//! (ids 0..=85) whose chunk table spans two sectors, so entry 84 straddles
//! the sector boundary and the reader's stitched entry parse runs against
//! real CD reads. Chunk 84 is an incompressible pattern (stored raw even
//! under `--world-pack-compress-rooms`); chunk 85 is a run pattern the
//! packer LZ4/HLZC-frames. Both patterns are recomputed here and must match
//! `tools/hello_pack_fixture.py` byte for byte.
//!
//! Steps, each shown as a line: prepare the reader, locate + load + FNV-check
//! + content-check the raw chunk, the same for the compressed chunk through
//! `load_chunk_decompressed`, and a lookup of an absent id. A green screen
//! with `ALL PASS` means table scan (straddle included), payload streaming,
//! checksums, and in-place decompression all work end-to-end off the disc.
//! A last line checks the reader put the interrupt mask back after its loads.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_pack::cd::{
    find_entry, load_chunk, load_chunk_decompressed, SectorReader, SECTOR_WORDS,
    WORLD_PACK_DEFAULT_LBA,
};
use psx_pack::fnv1a32;
use psx_rt::tty;
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

// Fixture layout, in lockstep with tools/hello_pack_fixture.py.
const RAW_ID: u32 = 84; // its table entry straddles header sectors 0/1
const RAW_LEN: usize = 3000; // 2 sectors, partial tail
const COMP_ID: u32 = 85;
const COMP_LEN: usize = 5000; // raw length; stored HLZC-compressed

/// Incompressible pattern for chunk 84: murmur3-finalizer avalanche of the
/// index. (A plain multiplicative hash is not enough; its high bits form a
/// near-arithmetic sequence and LZ4 shrinks it, turning the chunk HLZC.)
fn raw_byte(k: usize) -> u8 {
    let mut h = (k as u32).wrapping_add(RAW_ID.wrapping_mul(2_654_435_761));
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h as u8
}

/// Compressible pattern for chunk 85 (64-byte runs).
fn comp_byte(k: usize) -> u8 {
    (((k >> 6) as u32 + COMP_ID) & 0xFF) as u8
}

// The reader owns a one-sector bounce buffer and dst must hold the biggest
// raw chunk plus the in-place LZ4 margin, so keep them off the 32 KiB stack.
static mut READER: Option<SectorReader> = None;
static mut SCRATCH: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
const DST_WORDS: usize = 3072; // 12 KiB
static mut DST: [u32; DST_WORDS] = [0; DST_WORDS];

/// Up to this many result lines are recorded, then rendered every frame.
struct Report {
    lines: [Line; 12],
    len: usize,
    all_ok: bool,
    failed: &'static str,
}

#[derive(Copy, Clone)]
struct Line {
    text: &'static str,
    ok: bool,
}

impl Report {
    fn new() -> Self {
        Report {
            lines: [Line { text: "", ok: true }; 12],
            len: 0,
            all_ok: true,
            failed: "",
        }
    }

    /// Record one stage; echo it to TTY; `false` means stop the run.
    fn check(&mut self, label: &'static str, ok: bool) -> bool {
        if self.len < self.lines.len() {
            self.lines[self.len] = Line { text: label, ok };
            self.len += 1;
        }
        tty::print("hello-pack: ");
        tty::print(label);
        tty::println(if ok { " ok" } else { " FAIL" });
        if !ok {
            self.all_ok = false;
            if self.failed.is_empty() {
                self.failed = label;
            }
        }
        ok
    }
}

fn dst_bytes(dst: &[u32]) -> &[u8] {
    // SAFETY: [u32] viewed as its own bytes (little-endian target).
    unsafe { core::slice::from_raw_parts(dst.as_ptr() as *const u8, dst.len() * 4) }
}

fn bytes_match(bytes: &[u8], expect: fn(usize) -> u8) -> bool {
    let mut k = 0;
    while k < bytes.len() {
        if bytes[k] != expect(k) {
            return false;
        }
        k += 1;
    }
    true
}

/// Run the whole test and return the filled report.
fn run(rd: &mut SectorReader) -> Report {
    let mut rep = Report::new();
    // Set a mask bit no source raises here (SIO1), so the check at the end can
    // tell a restored mask from the VBlank-only one the reader runs under.
    psx_io::irq::set_mask(psx_io::irq::mask() | (1 << 8));
    let mask_before = psx_io::irq::mask();
    // SAFETY: main runs once; these statics are only touched here.
    let scratch = unsafe { &mut *addr_of_mut!(SCRATCH) };
    let dst = unsafe { &mut *addr_of_mut!(DST) };

    // The reader holds VBlank-only I_MASK from here to its last stop(), which
    // matches what psx_rt::interrupts installs for wait_vblank anyway.
    if !rep.check("PREPARE", rd.prepare()) {
        return rep;
    }

    // Raw chunk: its entry is the one that straddles header sectors 0/1.
    let Some(e84) = find_entry(rd, WORLD_PACK_DEFAULT_LBA, RAW_ID, scratch) else {
        rep.check("ENTRY 84", false);
        return rep;
    };
    if !rep.check(
        "ENTRY 84",
        e84.byte_size as usize == RAW_LEN && e84.sector_count == 2,
    ) {
        return rep;
    }
    let loaded = load_chunk(rd, WORLD_PACK_DEFAULT_LBA, RAW_ID, scratch, dst);
    if !rep.check("LOAD 84", loaded == Some(RAW_LEN)) {
        return rep;
    }
    if !rep.check(
        "FNV 84",
        fnv1a32(&dst_bytes(dst)[..RAW_LEN]) == e84.checksum,
    ) {
        return rep;
    }
    if !rep.check("DATA 84", bytes_match(&dst_bytes(dst)[..RAW_LEN], raw_byte)) {
        return rep;
    }

    // Compressed chunk: stored HLZC-framed, so the table byte_size must be
    // smaller than the raw length, and the stored bytes carry the checksum.
    let Some(e85) = find_entry(rd, WORLD_PACK_DEFAULT_LBA, COMP_ID, scratch) else {
        rep.check("ENTRY 85", false);
        return rep;
    };
    if !rep.check("ENTRY 85 HLZC", (e85.byte_size as usize) < COMP_LEN) {
        return rep;
    }
    let stored = load_chunk(rd, WORLD_PACK_DEFAULT_LBA, COMP_ID, scratch, dst);
    if !rep.check("LOAD 85", stored == Some(e85.byte_size as usize)) {
        return rep;
    }
    if !rep.check(
        "FNV 85",
        fnv1a32(&dst_bytes(dst)[..e85.byte_size as usize]) == e85.checksum,
    ) {
        return rep;
    }
    let raw = load_chunk_decompressed(rd, WORLD_PACK_DEFAULT_LBA, COMP_ID, scratch, dst);
    if !rep.check("LOAD-C 85", raw == Some(COMP_LEN)) {
        return rep;
    }
    if !rep.check(
        "DATA 85",
        bytes_match(&dst_bytes(dst)[..COMP_LEN], comp_byte),
    ) {
        return rep;
    }

    // A chunk id the pack does not contain must miss cleanly.
    let absent = find_entry(rd, WORLD_PACK_DEFAULT_LBA, 0xDEAD_BEEF, scratch).is_none();
    // Every load above ran under the reader's VBlank-only mask; stop() put the
    // one we set back. Read it before anything else runs: TTY output goes
    // through the BIOS, which the emulator's HLE leaves with I_MASK rewritten.
    let mask_after = psx_io::irq::mask();
    if !rep.check("ABSENT ID", absent) {
        return rep;
    }
    rep.check("IRQ MASK RESTORED", mask_after == mask_before);
    rep
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    // Run once at boot; hold the result on screen.
    // SAFETY: main runs once; the static is only touched here.
    let rd = unsafe { (*addr_of_mut!(READER)).insert(SectorReader::with_cd(peripherals.cd)) };
    let rep = run(rd);
    if rep.all_ok {
        tty::println("hello-pack: ALL PASS");
    } else {
        tty::print("hello-pack: FAIL ");
        tty::println(rep.failed);
    }

    loop {
        // Whole-screen verdict color so a headless --dump-hw is unambiguous.
        if rep.all_ok {
            fb.clear(&mut gpu, (16, 96, 32));
        } else {
            fb.clear(&mut gpu, (110, 20, 20));
        }
        font.draw_text(8, 6, "PSX-PACK CD STREAM TEST", (230, 230, 240));
        let mut y: i16 = 26;
        for i in 0..rep.len {
            let l = rep.lines[i];
            let tint = if l.ok { GREEN } else { RED };
            font.draw_text(8, y, if l.ok { "OK" } else { "XX" }, tint);
            font.draw_text(32, y, l.text, tint);
            y += 12;
        }
        if rep.all_ok {
            font.draw_text(8, 210, "ALL PASS", (235, 255, 235));
        } else {
            font.draw_text(8, 210, "FAIL", (255, 235, 235));
            font.draw_text(72, 210, rep.failed, (255, 235, 235));
        }

        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);
    }
}
