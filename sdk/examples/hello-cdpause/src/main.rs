// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-cdpause` -- the console's Pause, played against the emulator's model of it.
//!
//! Pause answers twice, and on the console the second answer (the drive has
//! stopped) comes 1.2 to 3.9 million system clocks after the first (see
//! `psx_io::cd::pause`). Three games read the status once after `stop()`,
//! saw "reading", and called a finished load a failure. This program runs the
//! same sequences with the emulator's silicon Pause model and checks:
//!
//! 1. `stop()` returns at the first response, with the second still to come
//!    (the status it leaves is printed, not judged);
//! 2. `stop_proved()` returns only when the drive says it has stopped;
//! 3. a Pause sent while the read is still seeking is refused (INT5) and
//!    `stop_proved()` waits the seek out and sends it again;
//! 4. a read started after any of these still delivers its sector.
//!
//! The disc carries `CDTEST.BIN` (`mkisopsx --cdtest-sectors`), found by name.
//! Results go to the TTY; the last line is `hello-cdpause: ALL PASS` or `FAIL`.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_fmv::iso;
use psx_hw::cd::{STAT_PLAYING, STAT_READING, STAT_SEEKING};
use psx_io::cd::reader::{SectorReader, DIAG_CD_ERROR, DIAG_PAUSE, SECTOR_WORDS};

const FILE_NAME: &str = "CDTEST.BIN";
/// Spins for one GetStat.
const STATUS_SPINS: u32 = 131_072;

static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut BAD: u32 = 0;

fn check(label: &str, ok: bool, detail: u32) {
    psx_rt::tty::print(label);
    psx_rt::tty::print(if ok { ": ok " } else { ": BAD " });
    print_num(detail);
    psx_rt::tty::print("\n");
    if !ok {
        // SAFETY: single threaded.
        unsafe { BAD += 1 };
    }
}

fn print_num(mut value: u32) {
    let mut digits = [0u8; 10];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    psx_rt::tty::print(core::str::from_utf8(&digits[at..]).unwrap_or("?"));
}

fn sector() -> &'static mut [u32; SECTOR_WORDS] {
    // SAFETY: single threaded; one borrow at a time.
    unsafe { &mut *core::ptr::addr_of_mut!(SECTOR) }
}

fn status(reader: &mut SectorReader) -> Option<u8> {
    reader
        .cd_mut()
        .try_status(STATUS_SPINS)
        .and_then(|r| r.bytes().first().copied())
}

fn read_n(reader: &mut SectorReader, lba: u32, count: u32) -> bool {
    if !reader.start_read(lba) {
        return false;
    }
    for _ in 0..count {
        if !reader.read_sector(sector()) {
            return false;
        }
    }
    true
}

fn file_lba(reader: &mut SectorReader) -> Option<(u32, u32)> {
    let read = |reader: &mut SectorReader, lba: u32| -> bool {
        reader.start_read(lba) && {
            let ok = reader.read_sector(sector());
            reader.stop();
            ok
        }
    };
    if !read(reader, iso::PVD_LBA) {
        return None;
    }
    let sector_bytes = |s: &[u32; SECTOR_WORDS]| -> &[u8] {
        // SAFETY: a `[u32; N]` viewed as its own bytes; alignment only shrinks.
        unsafe { core::slice::from_raw_parts(s.as_ptr().cast::<u8>(), 2048) }
    };
    let (root, _) = iso::root_directory(sector_bytes(sector()))?;
    if !read(reader, root) {
        return None;
    }
    iso::find_in_directory(sector_bytes(sector()), FILE_NAME)
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut reader = SectorReader::with_cd(peripherals.cd);
    if !reader.prepare() {
        psx_rt::tty::println("hello-cdpause: FAIL (prepare)");
        return;
    }
    let Some((lba, bytes)) = file_lba(&mut reader) else {
        psx_rt::tty::println("hello-cdpause: FAIL (CDTEST.BIN not found)");
        return;
    };
    let sectors = bytes / 2048;
    check("0 file", sectors >= 640, sectors);

    // 1. stop() returns at the acknowledge; the drive is still reading.
    let ok = read_n(&mut reader, lba, 12);
    reader.stop();
    let stat = status(&mut reader);
    check("1 stop() alone, 12 sectors read", ok, 0);
    // Informational: on the console a status asked 230,000 clocks after the
    // acknowledge says 0x02 already, long before the second response.
    check(
        "1 status after stop() (info)",
        stat.is_some(),
        stat.unwrap_or(0xFF) as u32,
    );

    // 2. stop_proved() waits for the stop.
    let ok = read_n(&mut reader, lba + 40, 12);
    let proved = reader.stop_proved();
    let stat = status(&mut reader);
    check("2 read for the proved stop", ok, 0);
    check("2 stop_proved is Ok", proved.is_ok(), reader.diagnostics());
    check(
        "2 the status then has no active bit",
        stat.is_some_and(|s| s & (STAT_READING | STAT_SEEKING | STAT_PLAYING) == 0),
        stat.unwrap_or(0xFF) as u32,
    );

    // 3. Pause sent while the read is still seeking: refused, then sent again.
    let far = lba + sectors - 8;
    let started = reader.start_read(far);
    let proved = reader.stop_proved();
    let diag = reader.diagnostics();
    let stat = status(&mut reader);
    check("3 read started at a far sector", started, 0);
    check(
        "3 stop_proved is Ok after the refusal",
        proved.is_ok(),
        diag,
    );
    check(
        "3 the drive refused the first Pause (INT5)",
        (diag >> 24) as u8 == DIAG_CD_ERROR,
        diag,
    );
    check(
        "3 the status then has no active bit",
        stat.is_some_and(|s| s & (STAT_READING | STAT_SEEKING | STAT_PLAYING) == 0),
        stat.unwrap_or(0xFF) as u32,
    );
    check(
        "3 no unproven-stop diagnostic",
        (diag >> 24) as u8 != DIAG_PAUSE,
        diag,
    );

    // 4. A read after all of it still delivers.
    let ok = read_n(&mut reader, lba + 100, 4);
    let proved = reader.stop_proved();
    check("4 a later read delivers", ok && proved.is_ok(), 0);

    // SAFETY: single threaded.
    let bad = unsafe { BAD };
    psx_rt::tty::println(if bad == 0 {
        "hello-cdpause: ALL PASS"
    } else {
        "hello-cdpause: FAIL"
    });
    loop {}
}
