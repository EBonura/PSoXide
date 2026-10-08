//! `hello-cdstream-probe` -- the console measurements the streaming design
//! needs, in one burnable disc, with no PC attached.
//!
//! It runs a fixed sequence through `psx-cdstream`, the production transport,
//! and shows what it measured on the screen in large text and as QR codes a
//! phone can read:
//!
//! 1. a seek table: one sector at 1, 16, 128, 512, 2048 and 8192 sectors from
//!    the previous read, forward and back, eight repeats each, min/median/max;
//! 2. the sustained read rate and the CPU the sector pops cost the foreground
//!    (a spin counter against an idle baseline) at double and single speed,
//!    and the longest interrupt handler (Timer 2);
//! 3. the time a lease request takes to stop a read in flight, and whether
//!    the motor winds down after a Pause;
//! 4. CD-DA next to data reads: Pause-to-idle, time to the first data sector
//!    after audio, time to resume audio at the saved position, the recovery
//!    Pause with audio still playing, and a bare read over playing audio.
//!    Four questions ask the listener (X = yes, O = no);
//! 5. what a Stop does to the next read, at once and after the motor has
//!    gone quiet (last, because a read right after Stop failed on a console);
//! 6. free RAM and stack depth of the probe itself.
//!
//! Times are HBlanks (63.56 us each; the display is NTSC 320x240) in the
//! payload and milliseconds on the screen. Run it on the emulator and the
//! numbers are the emulator's model, not the console's.
//!
//! Build the disc with `make hello-cdstream-probe-disc`; see the example's
//! README text in `docs` of that target for the burn and what to send back.

#![no_std]
#![no_main]

extern crate psx_rt;

mod clock;
mod drive;
mod phases;
mod report;
mod screen;

use core::ptr::{addr_of, read_volatile, write_volatile};
use psx_fmv::iso;
use psx_io::cd::reader::SectorReader;
use psx_io::periph::Cd;
use psx_rt::Peripherals;
use psx_spu::{self as spu, CdVolume, Spu, Volume};
use report::{report, Text};
use screen::Screen;

/// The benchmark file `make hello-cdstream-probe-disc` puts on the disc.
const FILE_NAME: &str = "CDEXTRA.BIN";
/// It must hold the farthest read the seek table makes.
const FILE_SECTORS_MIN: u32 = 8192 + 128;
const PATTERN: u32 = 0xA55A_5AA5;
/// Where the stack starts (`psoxide.ld`).
const STACK_TOP: usize = 0x801F_FF00;

extern "C" {
    static __bss_end: u8;
    static __text_start: u8;
    static __data_end: u8;
}

/// Roughly where `$sp` is: the address of a local.
#[inline(never)]
fn stack_pointer() -> usize {
    let marker = 0u32;
    core::hint::black_box(&marker) as *const u32 as usize
}

/// Paint the free RAM between `.bss` and the stack with a pattern, so the
/// deepest stack use can be read off at the end. Returns the painted range.
fn paint_free_ram() -> (usize, usize) {
    let low = (addr_of!(__bss_end) as usize + 3) & !3;
    let high = stack_pointer().saturating_sub(512) & !3;
    let mut at = low;
    while at < high {
        // SAFETY: RAM between the end of `.bss` and just below the live stack
        // frame, which nothing uses (the heap is off, the transport has its
        // own static stack).
        unsafe { write_volatile(at as *mut u32, PATTERN) };
        at += 4;
    }
    (low, high)
}

/// The lowest address of the painted range that something wrote to.
fn lowest_touched(range: (usize, usize)) -> usize {
    let mut at = range.0;
    while at < range.1 {
        // SAFETY: as `paint_free_ram`.
        if unsafe { read_volatile(at as *const u32) } != PATTERN {
            return at;
        }
        at += 4;
    }
    range.1
}

/// Find the benchmark file by name with the polled reader, then hand the
/// controller to the transport.
fn start(cd: Cd) -> Result<(), &'static str> {
    let mut reader = SectorReader::with_cd(cd);
    let mut sector = [0u32; drive::SECTOR_WORDS];
    if !reader.prepare() {
        return Err("READER PREPARE FAILED");
    }
    let found = lookup(&mut reader, &mut sector);
    let cd = reader.release();
    let Some((lba, bytes)) = found else {
        return Err("CDEXTRA.BIN NOT FOUND");
    };
    if bytes / 2048 < FILE_SECTORS_MIN {
        return Err("CDEXTRA.BIN TOO SMALL");
    }
    drive::set_file(lba, bytes / 2048);
    psx_rt::interrupts::wait_vblank();
    psx_cdstream::install(cd, drive::config(true, true)).map_err(|_| "ALREADY INSTALLED")
}

fn read_sector<'s>(
    reader: &mut SectorReader,
    sector: &'s mut [u32; drive::SECTOR_WORDS],
    lba: u32,
) -> Option<&'s [u8]> {
    if !reader.start_read(lba) {
        return None;
    }
    let ok = reader.read_sector(sector);
    reader.stop();
    // SAFETY: a `[u32; N]` viewed as its own bytes; alignment only shrinks.
    ok.then(|| unsafe { core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), 2048) })
}

fn lookup(
    reader: &mut SectorReader,
    sector: &mut [u32; drive::SECTOR_WORDS],
) -> Option<(u32, u32)> {
    let (root, _) = iso::root_directory(read_sector(reader, sector, iso::PVD_LBA)?)?;
    iso::find_in_directory(read_sector(reader, sector, root)?, FILE_NAME)
}

/// Memory page: image size, free RAM, and how deep the stack went.
fn memory_page(range: (usize, usize)) -> (u32, u32) {
    let text_start = addr_of!(__text_start) as usize;
    let data_end = addr_of!(__data_end) as usize;
    let bss_end = addr_of!(__bss_end) as usize;
    let low = lowest_touched(range);
    let stack_used = STACK_TOP - low;
    let free = low - bss_end;
    let handler_unused = psx_cdstream::handler_stack_unused_bytes() as u32;
    let r = report();
    r.page("7 MEMORY", true);
    let mut t = Text::new();
    t.s("IMAGE ")
        .u((data_end - text_start) as u32 / 1024)
        .s("K TEXT+DATA");
    r.line(&t);
    let mut t = Text::new();
    t.s("BSS ").u((bss_end - data_end) as u32 / 1024).s("K");
    r.line(&t);
    let mut t = Text::new();
    t.s("STACK DEEPEST ").u(stack_used as u32).s("B");
    r.line(&t);
    let mut t = Text::new();
    t.s("FREE ").u(free as u32 / 1024).s("K OF 2048K");
    r.line(&t);
    let mut t = Text::new();
    t.s("IRQ STACK FREE ").u(handler_unused).s("B");
    r.line(&t);
    r.kv(
        "M",
        &[
            text_start as u32,
            data_end as u32,
            bss_end as u32,
            low as u32,
            stack_used as u32,
            free as u32,
            handler_unused,
        ],
    );
    (free as u32 / 1024, stack_used as u32)
}

#[no_mangle]
fn main() {
    let Some(peripherals) = Peripherals::take() else {
        return;
    };
    let painted = paint_free_ram();
    let mut screen = Screen::new(peripherals.gpu_dma, peripherals.controller_port);
    let _spu = Spu::new(peripherals.spu_dma);
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);
    clock::init();

    screen.wait_start(
        "CD STREAM PROBE",
        &[
            "ABOUT THREE MINUTES.",
            "A TONE WILL PLAY:",
            "TURN THE SOUND UP.",
            "FOUR QUESTIONS WILL",
            "ASK YOU TO LISTEN.",
            "ANSWER WITH",
            "X = YES  O = NO.",
            "",
            "PRESS X OR START.",
        ],
    );
    if let Err(why) = start(peripherals.cd) {
        let r = report();
        r.page("PROBE COULD NOT START", false);
        let mut t = Text::new();
        t.s(why);
        r.line(&t);
        screen.browse(0);
    }

    phases::seek_table(&mut screen);
    screen.show_page(0, 180);
    phases::rate_and_cpu(&mut screen);
    screen.show_page(1, 180);
    phases::lease_and_pause(&mut screen);
    screen.show_page(2, 180);
    phases::cdda(&mut screen);
    screen.show_page(3, 120);
    screen.show_page(4, 120);
    phases::stop_tests(&mut screen);
    screen.show_page(5, 180);
    let (free_k, stack_bytes) = memory_page(painted);
    phases::summary(free_k, stack_bytes);

    // Quiet the transport before the BIOS writes the TTY.
    psx_cdstream::cancel_all();
    drive::wait_idle();
    let r = report();
    psx_rt::tty::println("hello-cdstream-probe: payload");
    psx_rt::tty::println(r.payload());
    for page in 0..r.page_total() {
        let mut line = 0;
        while let Some(text) = r.page_line(page, line) {
            psx_rt::tty::println(text);
            line += 1;
        }
    }
    psx_rt::tty::println("hello-cdstream-probe: done");
    // Open on the summary, the last report page.
    screen.browse(r.page_total() - 1);
}
