//! `cdda-read-contention` -- a focused PS1 CD-ROM conformance probe.
//!
//! It reproduces the exact situation cortex_ignition_v1 hits on real
//! hardware: menu CD-DA is playing, then gameplay issues a data read
//! WITHOUT pausing/stopping the drive first. The guest's `cd_stream`
//! reports `STATUS_CD_ERROR` there, which is an INT5 (Error) IRQ.
//!
//! This program drives the controller through the engine's real read
//! path and records WHICH IRQ the ReadN produces, so the same binary can
//! be compared across PSoXide, PCSX-Redux, DuckStation, and a console.
//!
//! Sequence (matches `engine/.../cd_stream/hw.rs`):
//!   SetMode CDDA|2x -> Demute -> Play(track 2)        [CD-DA, PLAYING]
//!   SetMode 2x (drop CDDA) -> SetLoc(data LBA) -> ReadN    [contention]
//!
//! Result is written to uncached RAM at `RESULT_BASE` (so an emulator
//! peek or a hardware probe sees it without cache games) and printed to
//! TTY. `RESULT_BASE+0` is the completion magic, written last.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_io::{read8, write32, write8};
use psx_rt::tty;

const CD_BASE: u32 = 0x1F80_1800;
const CD_STATUS: u32 = CD_BASE; // index select (write) / status (read)
const CD_CMD: u32 = CD_BASE + 1; // command (idx0 write) / response (read)
const CD_PARAM: u32 = CD_BASE + 2; // param push (idx0) / irq enable (idx1)
const CD_IRQ: u32 = CD_BASE + 3; // irq flag + ack (idx1)

const STAT_RESP_NOT_EMPTY: u8 = 1 << 5; // 0x20
const STAT_DATA_NOT_EMPTY: u8 = 1 << 6; // 0x40

const IRQ_DATA_READY: u8 = 1;
const IRQ_ERROR: u8 = 5;

// Uncached (KSEG1) RAM mirror so writes land in RAM immediately.
const RESULT_BASE: u32 = 0xA010_0000;
const DONE_MAGIC: u32 = 0x0C0D_0001;

const PLAY_TRACK: u8 = 2;
const DATA_LBA: u32 = 16;
const POLL_LIMIT: u32 = 4_000_000;

static SPIN_SINK: u32 = 0;

struct Resp {
    irq: u8,
    stat: u8,
}

#[no_mangle]
fn main() {
    unsafe {
        run();
    }
    loop {
        core::hint::spin_loop();
    }
}

unsafe fn run() {
    // Mask CDROM at the CPU IRQ controller and poll the controller flag
    // directly, exactly like the engine's polled read path.
    psx_io::irq::set_mask(1 << psx_io::irq::source::VBLANK);
    cd_set_index(1);
    write8(CD_PARAM, 0x00); // controller IRQ enable = 0 (poll only)
    write8(CD_IRQ, 0x1F); // ack any pending
    cd_set_index(0);

    write_result(0x14, 1);

    // 1) Menu-style CD-DA playback.
    let _ = cd_command(0x0E, &[0x81]); // SetMode: CDDA | DOUBLE_SPEED
    let _ = cd_command(0x0C, &[]); // Demute
    let _ = cd_command(0x03, &[bcd(PLAY_TRACK)]); // Play track 2

    // Let the drive settle into playback, then sample the stat byte.
    spin(2_000_000);
    let play = cd_command(0x01, &[]); // GetStat
    write_result(0x08, play.stat as u32);
    write_result(0x14, 2);

    // 2) Engine read path WHILE CD-DA plays, with no Pause/Stop first.
    let _ = cd_command(0x0E, &[0x80]); // SetMode: DOUBLE_SPEED only (drop CDDA)
    let (m, s, f) = lba_to_bcd_msf(DATA_LBA);
    let _ = cd_command(0x02, &[m, s, f]); // SetLoc(data sector)
    write_result(0x14, 3);

    // Issue ReadN and capture EVERY IRQ it produces.
    cd_ack_all();
    cd_set_index(0);
    drain_responses();
    write8(CD_CMD, 0x06); // ReadN

    let mut irq_seen = 0u32;
    let mut last_stat = 0u8;
    let mut data_ready = 0u32;
    let mut polls = 0;
    while polls < 6 {
        let flag = poll_irq(POLL_LIMIT);
        if flag == 0 {
            break;
        }
        irq_seen |= 1u32 << (flag & 0x7);
        last_stat = read_first_response();
        if cd_status() & STAT_DATA_NOT_EMPTY != 0 {
            data_ready = 1;
        }
        cd_ack(flag);
        // A data-ready or an error settles the question.
        if flag == IRQ_DATA_READY || flag == IRQ_ERROR {
            break;
        }
        polls += 1;
    }
    if cd_status() & STAT_DATA_NOT_EMPTY != 0 {
        data_ready = 1;
    }

    write_result(0x04, irq_seen);
    write_result(0x0C, last_stat as u32);
    write_result(0x10, data_ready);
    write_result(0x14, 4);

    // Human/harness-readable summary.
    tty::print("CDDA-CONTENTION irq=0x");
    tty::print_hex_u32(irq_seen);
    tty::print(" play_stat=0x");
    tty::print_hex_u32(play.stat as u32);
    tty::print(" read_stat=0x");
    tty::print_hex_u32(last_stat as u32);
    tty::print(" data=0x");
    tty::print_hex_u32(data_ready);
    if irq_seen & (1 << IRQ_ERROR) != 0 {
        tty::println(" RESULT=INT5_ERROR");
    } else if irq_seen & (1 << IRQ_DATA_READY) != 0 {
        tty::println(" RESULT=DATA_DELIVERED");
    } else {
        tty::println(" RESULT=UNKNOWN");
    }

    // Completion marker LAST so a reader can poll RESULT_BASE+0.
    write_result(0x00, DONE_MAGIC);
}

unsafe fn cd_command(cmd: u8, params: &[u8]) -> Resp {
    cd_ack_all();
    cd_set_index(0);
    drain_responses();
    for &p in params {
        write8(CD_PARAM, p);
    }
    write8(CD_CMD, cmd);
    let irq = poll_irq(POLL_LIMIT); // INT3 ack, or INT5 error
    let stat = read_first_response();
    cd_ack(irq);
    Resp { irq, stat }
}

unsafe fn poll_irq(limit: u32) -> u8 {
    let mut i = 0u32;
    loop {
        cd_set_index(1);
        let flag = read8(CD_IRQ) & 0x1F;
        cd_set_index(0);
        if flag != 0 {
            return flag;
        }
        if i >= limit {
            return 0;
        }
        i += 1;
    }
}

unsafe fn read_first_response() -> u8 {
    cd_set_index(0);
    let mut first = 0u8;
    let mut got = false;
    while cd_status() & STAT_RESP_NOT_EMPTY != 0 {
        let b = read8(CD_CMD);
        if !got {
            first = b;
            got = true;
        }
    }
    first
}

unsafe fn drain_responses() {
    let _ = read_first_response();
}

unsafe fn cd_ack(bits: u8) {
    cd_set_index(1);
    write8(CD_IRQ, bits & 0x1F);
    cd_set_index(0);
}

unsafe fn cd_ack_all() {
    cd_ack(0x1F);
}

unsafe fn cd_status() -> u8 {
    read8(CD_STATUS)
}

unsafe fn cd_set_index(index: u8) {
    write8(CD_STATUS, index & 0x03);
}

unsafe fn write_result(offset: u32, value: u32) {
    write32(RESULT_BASE + offset, value);
}

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn lba_to_bcd_msf(lba: u32) -> (u8, u8, u8) {
    let total = lba + 150;
    let m = (total / (75 * 60)) as u8;
    let s = ((total / 75) % 60) as u8;
    let f = (total % 75) as u8;
    (bcd(m), bcd(s), bcd(f))
}

fn spin(n: u32) {
    let mut i = 0u32;
    while i < n {
        unsafe {
            core::ptr::read_volatile(&SPIN_SINK);
        }
        i += 1;
    }
}
