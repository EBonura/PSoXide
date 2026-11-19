//! `hello-cdlean` -- the smallest transport, `psx_cdstream::lean`: no counters, no tickets. The
//! probe for a game that starts a read, asks once a frame whether it is done, and cancels or
//! waits for the drive before polled reads.
//!
//! It finds `CDTEST.BIN` with the polled reader (`make hello-cdlean-disc`), installs
//! `psx_cdstream::lean` with the CD source open only while a request runs, then:
//!
//! 1. streams a chunk that ends inside a sector (`with_byte_len`), looking at it once every six
//!    VBlanks as a split-screen frame loop does, and checks the bytes, that nothing past the
//!    chunk was written, and that `status` was `Busy` until the drive had stopped;
//! 2. starts another chunk, cancels it, and waits for the drive to stop (`stop_and_wait`), and
//!    checks that the status is then `Failed` and that a refused second `submit` did not disturb it;
//! 3. reads the directory again with the polled reader, as the loader does between chunks.
//!
//! The verdict goes to the TTY only: this program draws nothing, so its image is the transport
//! and the checks.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_cdstream::{lean, Config, IrqMaskPolicy, Request, Status};
use psx_io::cd::reader::SectorReader;
use psx_rt::interrupts::{vblank_count, wait_vblank};

/// The file the disc is built with (`--cdtest-sectors`), found by name.
const FILE_NAME: &str = "CDTEST.BIN";
/// VBlanks to wait for a request before giving up.
const PATIENCE_VBLANKS: u32 = 600;
const SECTOR_WORDS: usize = 512;
/// The chunk: three sectors and a bit, so the last sector is stored only in part.
const CHUNK_SECTORS: u32 = 4;
const CHUNK_BYTES: u32 = 3 * 2048 + 100;
const CANARY: u32 = 0xdead_beef;

static mut CHUNK: [u32; CHUNK_SECTORS as usize * SECTOR_WORDS] =
    [0; CHUNK_SECTORS as usize * SECTOR_WORDS];

/// Word `word` of the file, as `psx_iso::cd_stream_bench_expected_byte` defines its bytes (a
/// mirror, so the guest needs no host crate).
fn expected_word(word: u32, sectors: u32) -> u32 {
    let byte = |index: u32| -> u32 {
        const MAGIC: &[u8; 8] = b"PSOXSTRM";
        u32::from(if index < 8 {
            MAGIC[index as usize]
        } else if index < 12 {
            sectors.to_le_bytes()[(index - 8) as usize]
        } else {
            index
                .wrapping_mul(37)
                .wrapping_add(index >> 3)
                .wrapping_add(0x5d) as u8
        })
    };
    let at = word * 4;
    byte(at) | byte(at + 1) << 8 | byte(at + 2) << 16 | byte(at + 3) << 24
}

fn chunk() -> *mut u32 {
    addr_of_mut!(CHUNK).cast()
}

/// Whether the chunk holds file words `first..first + words` and the canary after them.
fn chunk_is(first: u32, words: u32, file_sectors: u32) -> bool {
    let mut ok = true;
    for word in 0..CHUNK_SECTORS * SECTOR_WORDS as u32 {
        // SAFETY: inside `CHUNK`; the request that filled it has finished.
        let got = unsafe { chunk().add(word as usize).read_volatile() };
        let want = if word < words {
            expected_word(first + word, file_sectors)
        } else {
            CANARY
        };
        ok &= got == want;
    }
    ok
}

fn main_checks(lba: u32, file_sectors: u32) -> bool {
    // 1. a chunk, looked at every six VBlanks.
    for word in 0..CHUNK_SECTORS as usize * SECTOR_WORDS {
        // SAFETY: inside `CHUNK`; nothing is running.
        unsafe { chunk().add(word).write_volatile(CANARY) };
    }
    // SAFETY: `CHUNK` is `CHUNK_SECTORS` sectors and nothing touches it until the request ends.
    let request =
        unsafe { Request::new_raw(lba + 16, CHUNK_SECTORS, chunk()) }.with_byte_len(CHUNK_BYTES);
    if lean::submit(request).is_err() {
        return false;
    }
    // A second request while the first runs is refused and leaves it alone.
    // SAFETY: refused, so the pointer is never used.
    let other = unsafe { Request::new_raw(lba + 100, 1, chunk()) };
    let refused = lean::submit(other).is_err();
    let start = vblank_count();
    let mut idle_while_done = false;
    let status = loop {
        for _ in 0..6 {
            wait_vblank();
        }
        match lean::status() {
            Status::Busy if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS => {
                break Status::Busy
            }
            Status::Busy => {}
            done => {
                // `Done` is reported only once the drive has stopped.
                idle_while_done = lean::is_idle();
                break done;
            }
        }
    };
    let whole = status == Status::Done && idle_while_done;
    let bytes_ok = chunk_is(
        16 * SECTOR_WORDS as u32,
        CHUNK_BYTES.div_ceil(4),
        file_sectors,
    );

    // 2. a chunk cancelled at once, then the drive stopped.
    // SAFETY: as above; the first request has finished and the drive has stopped.
    let again = unsafe { Request::new_raw(lba + 100, CHUNK_SECTORS, chunk()) };
    let second = lean::submit(again).is_ok();
    let stopped = lean::stop_and_wait(120);
    let cancelled = second && lean::status() == Status::Failed;

    // 3. the polled reader, between chunks.
    // SAFETY: nothing is queued or active and the source is closed (`OnlyWhileBusy`), so the
    // transport is not programming the controller.
    let polled = unsafe { psx_io::periph::Cd::steal() };
    let mut reader = SectorReader::with_cd(polled);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = reader.prepare() && reader.find(&mut sector, None, &[FILE_NAME]).is_some();
    let _ = reader.release();
    refused && whole && bytes_ok && stopped && cancelled && found
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut reader = SectorReader::with_cd(peripherals.cd);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = if reader.prepare() {
        reader.find(&mut sector, None, &[FILE_NAME])
    } else {
        None
    };
    let cd = reader.release();
    let ok = match found {
        Some((lba, bytes)) => {
            wait_vblank();
            let config = Config {
                timeout_vblanks: 300,
                irq_mask_policy: IrqMaskPolicy::OnlyWhileBusy,
                ..Config::DEFAULT
            };
            lean::install(cd, config).is_ok() && main_checks(lba, bytes / 2048)
        }
        None => false,
    };
    let _ = lean::stop_and_wait(120);
    psx_rt::tty::println(if ok {
        "hello-cdlean: ALL PASS"
    } else {
        "hello-cdlean: FAIL"
    });
    loop {
        wait_vblank();
    }
}
