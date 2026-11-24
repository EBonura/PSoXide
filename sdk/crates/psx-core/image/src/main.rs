// SPDX-License-Identifier: GPL-2.0-or-later
//! The scene core's image: everything that runs from the window 0x801FDF00..0x801FFF00.
//!
//! Built alone (`image.ld`), flattened to `psx-core.bin` and embedded byte for byte in every
//! program on the core. Sections, in the order a size report lists them:
//! - `cmd`: the CD command layer (send and wait, response FIFO, bounded drain, acknowledge);
//! - `drive`: the polled reader and its rules: never assume an idle drive (settle on first use),
//!   seek-first starts, PIO sector pops (no CD DMA), Pause and never Stop, a Pause resent when
//!   refused mid-seek, the CD input muted before a read after music, and the drive proven stopped
//!   before every command the core sends: a read or `psx_core_pause` ends on an acknowledged
//!   Pause and the next use proves it (`drive::settle`), so the wait overlaps the program's work;
//! - `read`: the read loop with retries from a fresh start and contiguous chunks;
//! - `switch`: the table row, bounds, quiesce, load with the checksum folded in, I-cache flush,
//!   handoff block and jump;
//! - `fatal`: the screen, shared with the program side in `psx_core::fatal`;
//! - the entry table and the statics.
//!
//! Every wait is bounded by time on root counter 2, read and never written, with a spin
//! backstop, as psx-io's polled reader does (its module docs carry the console measurements
//! the rules come from).
#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![feature(optimize_attribute)]

use psx_core::abi::{self, block, code, table};
use psx_core::fnv;

// ---------------------------------------------------------------------------------------------
// Registers.

const CD_INDEX: u32 = 0x1f80_1800;
const CD_CMD: u32 = 0x1f80_1801;
const CD_PARAM: u32 = 0x1f80_1802;
const CD_REQ: u32 = 0x1f80_1803;
const I_STAT: u32 = 0x1f80_1070;
const I_MASK: u32 = 0x1f80_1074;
const TIMER2: u32 = 0x1f80_1120;
const DPCR: u32 = 0x1f80_10f0;
const DICR: u32 = 0x1f80_10f4;
const DMA_CHCR0: u32 = 0x1f80_1088;
const SPU_KEY_OFF: u32 = 0x1f80_1d8c;
const SPU_CD_VOLUME: u32 = 0x1f80_1db0;
const SPU_CONTROL: u32 = 0x1f80_1daa;

const CDROM_IRQ_BIT: u32 = 1 << 2;

const ST_PARAM_NOT_FULL: u8 = 1 << 4;
const ST_RESPONSE: u8 = 1 << 5;
const ST_DATA: u8 = 1 << 6;

const INT_DATA_READY: u8 = 1;
const INT_COMPLETE: u8 = 2;
const INT_ACK: u8 = 3;
const INT_ERROR: u8 = 5;

const CMD_GETSTAT: u8 = 0x01;
const CMD_SETLOC: u8 = 0x02;
const CMD_READN: u8 = 0x06;
const CMD_PAUSE: u8 = 0x09;
const CMD_SETMODE: u8 = 0x0e;
const CMD_SEEKL: u8 = 0x15;

/// Double speed, 2048-byte user data.
const MODE_DATA: u8 = 0x80;
/// Status bits of a drive that is doing something.
const STAT_ACTIVE: u8 = 0x80 | 0x40 | 0x20;

const CLOCKS_PER_MS: u32 = 33_869;
const ACK_WAIT: u32 = 200 * CLOCKS_PER_MS;
const FIRST_SECTOR_WAIT: u32 = 10_000 * CLOCKS_PER_MS;
const NEXT_SECTOR_WAIT: u32 = 2_000 * CLOCKS_PER_MS;
const FIFO_WAIT: u32 = 100 * CLOCKS_PER_MS;
const PAUSE_COMPLETE_WAIT: u32 = 700 * CLOCKS_PER_MS;
const SEEK_END_WAIT: u32 = 1_000 * CLOCKS_PER_MS;
/// A step of the counter is counted at most this many clocks (a wrap cannot end a wait).
const MAX_STEP: u32 = 0x4000;
/// The spin backstop: at most one spin per this many clocks of a bound.
const MIN_SPIN_CLOCKS: u32 = 8;
/// GetStats asked when a Pause's second response never came.
const STATUS_POLLS: u32 = 1_000;
const PAUSE_ATTEMPTS: u32 = 3;
const DRAIN_LIMIT: u32 = 256;
const PARAM_POLL: u32 = 16_384;
/// Sectors per seek-first read start: long implicit-seek streams corrupted on a console
/// (psx-io reader.rs), so every chunk starts with its own SeekL.
const CHUNK_SECTORS: u32 = 256;
const DEFAULT_ATTEMPTS: u32 = 3;
const SECTOR_WORDS: u32 = 512;

#[inline(always)]
fn r8(addr: u32) -> u8 {
    // SAFETY: fixed MMIO register.
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}
#[inline(always)]
fn w8(addr: u32, v: u8) {
    // SAFETY: fixed MMIO register.
    unsafe { core::ptr::write_volatile(addr as *mut u8, v) }
}
#[inline(always)]
fn r16(addr: u32) -> u16 {
    // SAFETY: fixed MMIO register.
    unsafe { core::ptr::read_volatile(addr as *const u16) }
}
#[inline(always)]
fn w16(addr: u32, v: u16) {
    // SAFETY: fixed MMIO register.
    unsafe { core::ptr::write_volatile(addr as *mut u16, v) }
}
#[inline(always)]
fn r32(addr: u32) -> u32 {
    // SAFETY: fixed MMIO register or core-owned RAM.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
#[inline(always)]
fn w32(addr: u32, v: u32) {
    // SAFETY: fixed MMIO register or core-owned RAM.
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

// ---------------------------------------------------------------------------------------------
// Statics: zeroed by `__core_boot` on a cold entry, kept across switches.

/// `State::prepared`: nothing known of the drive yet (zeroed statics).
const UNKNOWN: u32 = 0;
/// `State::prepared`: the drive is proven stopped, or doing what the core last started.
const PROVEN: u32 = 1;
/// `State::prepared`: a Pause was acknowledged and its proof is owed to the next use.
const OWED: u32 = 2;

#[repr(C)]
struct State {
    // These ten words are the handoff block's 0x08..0x30, in its order: `write_block` copies them.
    /// The running core's hash, written by a cold attach (never 0: psx-core asserts it), so a
    /// core entry that finds 0 has no live core.
    hash: u32,
    lba_offset: u32,
    cdda_base: u32,
    /// `current | previous << 16` as last written.
    scenes: u32,
    /// The block's flags as last written.
    block_flags: u32,
    switches: u32,
    /// Arguments of the switch in flight.
    args: [u32; 4],
    current: u32,
    flags: u32,
    /// A stream client holds the drive.
    client: u32,
    /// What the core knows of the drive: [`UNKNOWN`], [`PROVEN`] stopped or idle, or a Pause
    /// acknowledged whose proof is [`OWED`] to the next use (`drive::settle`).
    prepared: u32,
    /// `I_MASK` as the caller had it while a polled read runs.
    saved_mask: u32,
    /// First response byte of the last command that matched.
    last_status: u32,
    /// The running read has not delivered a sector yet.
    first_sector: u32,
    /// `I_MASK` is saved in `saved_mask` (a read or a stream is open).
    mask_saved: u32,
    /// The program's idle hook, called from every spin of every wait.
    idle: Option<extern "C" fn()>,
    /// Stage of the last fatal screen.
    fatal: u32,
    /// The scene row's first two words (id, image and flags; entry PC) and the image row of the
    /// switch or load in flight: copied, the caller's table may be overwritten by the load.
    scene: [u32; 2],
    image: [u32; 8],
    /// The image index of the rows in flight.
    img: u32,
    /// One bit per image of the table that is in RAM now.
    mask: u32,
    /// A cold boot has not yet added the overlays the boot program's exe carries to `mask`
    /// (the first table the program passes has them).
    carry_pending: u32,
}

static mut ST: State = State {
    hash: 0,
    lba_offset: 0,
    cdda_base: 0,
    scenes: 0,
    block_flags: 0,
    switches: 0,
    args: [0; 4],
    current: 0,
    flags: 0,
    client: 0,
    prepared: UNKNOWN,
    saved_mask: 0,
    last_status: 0,
    first_sector: 0,
    mask_saved: 0,
    idle: None,
    fatal: 0,
    scene: [0; 2],
    image: [0; 8],
    img: 0,
    mask: 0,
    carry_pending: 0,
};

#[inline(always)]
fn st() -> &'static mut State {
    // SAFETY: single-threaded; no interrupt handler touches the core's statics.
    unsafe { &mut *core::ptr::addr_of_mut!(ST) }
}

// ---------------------------------------------------------------------------------------------
// Bounded waits on root counter 2.

struct Elapsed {
    last: u32,
    clocks: u32,
    spins: u32,
}

impl Elapsed {
    #[inline(always)]
    fn start() -> Self {
        Elapsed {
            last: r16(TIMER2) as u32,
            clocks: 0,
            spins: 0,
        }
    }

    /// One spin; `true` once `limit` clocks (or the spin backstop) have passed.
    #[inline(never)]
    fn step(&mut self, limit: u32) -> bool {
        let now = r16(TIMER2) as u32;
        let step = (now.wrapping_sub(self.last) & 0xffff).min(MAX_STEP);
        self.last = now;
        self.clocks = self.clocks.saturating_add(step);
        self.spins += 1;
        self.clocks >= limit || self.spins >= limit / MIN_SPIN_CLOCKS
    }
}

// ---------------------------------------------------------------------------------------------
// cmd: the command layer.

mod cmd {
    use super::*;

    #[inline(always)]
    pub fn index(i: u8) {
        w8(CD_INDEX, i);
    }

    #[inline(never)]
    pub fn flag() -> u8 {
        index(1);
        let f = r8(CD_REQ) & 0x1f;
        index(0);
        f
    }

    #[inline(never)]
    pub fn ack(bits: u8) {
        index(1);
        w8(CD_REQ, (bits | 0x07) & 0x1f);
        w32(I_STAT, !CDROM_IRQ_BIT);
        index(0);
    }

    /// Acknowledge every controller IRQ, then the CD-ROM bit of `I_STAT`.
    #[inline(never)]
    pub fn ack_all() {
        index(1);
        w8(CD_REQ, 0x1f);
        w32(I_STAT, !CDROM_IRQ_BIT);
        index(0);
    }

    #[inline(never)]
    pub fn drain() {
        index(0);
        let mut n = 0;
        while r8(CD_INDEX) & ST_RESPONSE != 0 && n < DRAIN_LIMIT {
            let _ = r8(CD_CMD);
            n += 1;
        }
    }

    pub fn irq_enable() -> u8 {
        index(0);
        r8(CD_REQ) & 0x1f
    }

    pub fn set_irq_enable(mask: u8) {
        index(1);
        w8(CD_PARAM, mask & 0x1f);
        index(0);
    }

    #[inline(always)]
    pub fn data_ready() -> bool {
        index(0);
        r8(CD_INDEX) & ST_DATA != 0
    }

    pub fn clear_data_request() {
        index(0);
        w8(CD_REQ, 0);
    }

    /// Wait outcomes.
    pub const MATCHED: u32 = 0;
    pub const CD_ERROR: u32 = 1;
    pub const TIMEOUT: u32 = 2;

    /// Wait up to `limit` clocks for `expected`, acknowledging (and for a stale DataReady
    /// draining) anything else on the way.
    #[inline(never)]
    pub fn wait(expected: u8, limit: u32) -> u32 {
        let mut e = Elapsed::start();
        loop {
            if let Some(hook) = st().idle {
                hook();
            }
            let f = flag();
            // Some drives raise the data FIFO before, or without, the DataReady flag.
            if f == expected || (expected == INT_DATA_READY && data_ready()) {
                return MATCHED;
            }
            if f == INT_ERROR {
                return CD_ERROR;
            }
            if f != 0 {
                drive::ack_unexpected(f);
            }
            if e.step(limit) {
                return TIMEOUT;
            }
        }
    }

    /// Send `command` with `params`, controller IRQs masked and every FIFO in a known state, and
    /// wait for `expected`. The mask and acknowledge order is the one proven on silicon.
    #[inline(never)]
    pub fn send(command: u8, params: &[u8], expected: u8, limit: u32) -> u32 {
        send_with(command, params, expected, limit, core::ptr::null_mut()) & 0xff
    }

    /// [`send`] for a raw command: waits for the acknowledge and copies its response.
    pub fn send_collect(command: u8, params: &[u8], resp: *mut u8) -> u32 {
        send_with(command, params, INT_ACK, ACK_WAIT, resp)
    }

    /// Empty the parameter FIFO, push the parameters and write the command byte; no wait (also
    /// the raw slot's op 1).
    #[inline(never)]
    pub fn dispatch(command: u8, params: &[u8]) -> u32 {
        index(1);
        w8(CD_REQ, 0x40);
        index(0);
        for &p in params {
            let mut spins = PARAM_POLL;
            while r8(CD_INDEX) & ST_PARAM_NOT_FULL == 0 {
                if spins == 0 {
                    return TIMEOUT;
                }
                spins -= 1;
            }
            w8(CD_PARAM, p);
        }
        w8(CD_CMD, command);
        MATCHED
    }

    /// Pop the response FIFO into `resp` (16 bytes at most); the length.
    pub fn collect(resp: *mut u8) -> u32 {
        index(0);
        let mut len = 0u32;
        while r8(CD_INDEX) & ST_RESPONSE != 0 && len < 16 {
            let b = r8(CD_CMD);
            if len == 0 {
                st().last_status = b as u32;
            }
            if !resp.is_null() {
                // SAFETY: the caller's 16-byte response buffer.
                unsafe { core::ptr::write_volatile(resp.add(len as usize), b) };
            }
            len += 1;
        }
        len
    }

    #[inline(never)]
    fn send_with(command: u8, params: &[u8], expected: u8, limit: u32, resp: *mut u8) -> u32 {
        let saved = irq_enable();
        set_irq_enable(0);
        ack_all();
        drain();
        if dispatch(command, params) != MATCHED {
            set_irq_enable(saved);
            return TIMEOUT;
        }
        let r = wait(expected, limit);
        match r {
            MATCHED => {
                let len = collect(resp);
                drain();
                ack(expected);
                set_irq_enable(saved);
                return MATCHED | (len << 8);
            }
            CD_ERROR => {
                drain();
                ack_all();
            }
            _ => {}
        }
        set_irq_enable(saved);
        r
    }
}

// ---------------------------------------------------------------------------------------------
// drive: the polled reader and its rules.

mod drive {
    use super::*;

    /// Pop one sector by PIO into `dst` (or into a register when `dst` is null), folding every
    /// word into `*hash` unless `hash` is null (a stream sector). `false` when the FIFO
    /// never filled (the request is dropped, so a late sector cannot land in the next read).
    #[inline(never)]
    #[optimize(speed)]
    pub fn pop(dst: *mut u32, hash: *mut u32) -> bool {
        w8(CD_REQ, 0x80);
        let mut e = Elapsed::start();
        while !cmd::data_ready() {
            if e.step(FIFO_WAIT) {
                cmd::clear_data_request();
                return false;
            }
        }
        if dst.is_null() {
            // A stale sector: drained into a register.
            let mut n = 0;
            while n < SECTOR_WORDS * 4 {
                let _ = r8(CD_PARAM);
                n += 1;
            }
            return true;
        }
        let plain = hash.is_null();
        // SAFETY: a non-null `hash` is the caller's running checksum.
        let mut h = if plain { 0 } else { unsafe { *hash } };
        let mut at = dst;
        let end = dst.wrapping_add(SECTOR_WORDS as usize);
        while at != end {
            let b0 = r8(CD_PARAM) as u32;
            let b1 = r8(CD_PARAM) as u32;
            let b2 = r8(CD_PARAM) as u32;
            let b3 = r8(CD_PARAM) as u32;
            let word = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0;
            // SAFETY: the caller's destination covers a whole sector.
            unsafe { core::ptr::write_volatile(at, word) };
            at = at.wrapping_add(1);
            if plain {
                // A stream client's sector: no checksum, the shape of psx-io's reader.
                continue;
            }
            // The FNV prime as shifts and adds (0x0100_0193 = 2^24 + 2^8 + 2^7 + 2^4 + 2^1 + 1):
            // no `mult`, so no wait on `mflo` while the next bytes could be popped.
            let x = h ^ word;
            let s: u32;
            // SAFETY: register arithmetic only.
            unsafe {
                core::arch::asm!(
                    ".set noat",
                    "sll {t}, {x}, 1",
                    "addu {s}, {x}, {t}",
                    "sll {t}, {x}, 4",
                    "addu {s}, {s}, {t}",
                    "sll {t}, {x}, 7",
                    "addu {s}, {s}, {t}",
                    "sll {t}, {x}, 8",
                    "addu {s}, {s}, {t}",
                    "sll {t}, {x}, 24",
                    "addu {s}, {s}, {t}",
                    ".set at",
                    x = in(reg) x, s = out(reg) s, t = out(reg) _,
                    options(pure, nomem, nostack)
                )
            };
            h = s;
        }
        if !plain {
            // SAFETY: as above.
            unsafe { *hash = h };
        }
        true
    }

    /// Clear an IRQ nobody waited for. A stale DataReady has its sector drained first, or the
    /// FIFO stays occupied and later reads misalign.
    #[inline(never)]
    pub fn ack_unexpected(f: u8) {
        if f == INT_DATA_READY {
            let _ = pop(core::ptr::null_mut(), core::ptr::null_mut());
        }
        cmd::drain();
        if f <= 4 {
            cmd::ack(f);
        } else {
            cmd::ack_all();
        }
    }

    #[inline(never)]
    pub fn status() -> Option<u8> {
        if cmd::send(CMD_GETSTAT, &[], INT_ACK, ACK_WAIT) == cmd::MATCHED {
            Some(st().last_status as u8)
        } else {
            None
        }
    }

    /// After a refused Pause: wait for the read's first sector, which ends its seek.
    fn wait_seek_end() {
        match cmd::wait(INT_DATA_READY, SEEK_END_WAIT) {
            cmd::MATCHED => ack_unexpected(INT_DATA_READY),
            cmd::CD_ERROR => {
                cmd::drain();
                cmd::ack_all();
            }
            _ => {}
        }
    }

    /// Pause the drive and return only once it has stopped: INT2, or failing that a status with
    /// no active bit. A refusal mid-seek waits the seek out and sends again.
    #[inline(never)]
    pub fn pause_proved() -> u32 {
        let mut attempt = 0;
        while attempt < PAUSE_ATTEMPTS {
            match cmd::send(CMD_PAUSE, &[], INT_ACK, ACK_WAIT) {
                cmd::MATCHED => match cmd::wait(INT_COMPLETE, PAUSE_COMPLETE_WAIT) {
                    cmd::MATCHED => {
                        cmd::drain();
                        cmd::ack(INT_COMPLETE);
                        return code::OK;
                    }
                    cmd::CD_ERROR => {
                        cmd::drain();
                        cmd::ack_all();
                    }
                    _ => {
                        let mut polls = 0;
                        while polls < STATUS_POLLS {
                            match status() {
                                Some(s) if s & STAT_ACTIVE == 0 => return code::OK,
                                Some(_) => {}
                                None => return code::PAUSE,
                            }
                            polls += 1;
                        }
                        return code::PAUSE;
                    }
                },
                cmd::CD_ERROR => wait_seek_end(),
                _ => return code::NO_ANSWER,
            }
            attempt += 1;
        }
        code::PAUSE
    }

    /// The acknowledge-only stop, between chunks, at the end of a read and at `psx_core_pause`:
    /// Pause until the drive takes it (once more after a refusal mid-seek), without waiting for
    /// it to stop. Its proof is owed to the next use of the drive ([`settle`]), so the drive
    /// finishes stopping while the program works on what it read. `MATCHED` once acknowledged.
    #[inline(never)]
    pub fn stop() -> u32 {
        let mut r = cmd::send(CMD_PAUSE, &[], INT_ACK, ACK_WAIT);
        if r == cmd::CD_ERROR {
            wait_seek_end();
            r = cmd::send(CMD_PAUSE, &[], INT_ACK, ACK_WAIT);
        }
        cmd::ack_all();
        st().prepared = OWED;
        r
    }

    /// Prove the drive stopped before the core's own next command. On the first use, whatever
    /// the drive is doing (it is never assumed idle); after [`stop`], that Pause: by its
    /// second response if it has latched (no command), else a status and a proven Pause while
    /// the drive is still active. Never a blocking wait for that latched response: our own
    /// acknowledges, or a program's register-level driver, may have taken it.
    #[inline(never)]
    pub fn settle() -> u32 {
        let s = st();
        if s.prepared == PROVEN {
            return code::OK;
        }
        if s.prepared != OWED || cmd::flag() != INT_COMPLETE {
            let mut n = 0;
            while n < 16 {
                let f = cmd::flag();
                if f == 0 {
                    break;
                }
                ack_unexpected(f);
                n += 1;
            }
            cmd::ack_all();
            match status() {
                // No answer: the next command fails on its own bound and says so.
                None => {}
                Some(stat) if stat & STAT_ACTIVE == 0 => {}
                Some(_) => {
                    let r = pause_proved();
                    if r != code::OK {
                        cmd::ack_all();
                        return r;
                    }
                }
            }
        }
        // The latched second response's status byte too: nothing is left pending.
        cmd::drain();
        cmd::ack_all();
        s.prepared = PROVEN;
        code::OK
    }

    /// Take the controller for polled reads: the CD source out of `I_MASK` until [`release`],
    /// the drive proven stopped ([`settle`]), controller IRQs on.
    #[inline(never)]
    pub fn prepare() -> u32 {
        enter_mask();
        // Before anything is acknowledged: an owed Pause's latched second response is its proof.
        let r = settle();
        if r != code::OK {
            return r;
        }
        w32(I_STAT, !CDROM_IRQ_BIT);
        cmd::set_irq_enable(0x1f);
        cmd::ack_all();
        // Drop whatever the previous tenant left in the data FIFO.
        cmd::clear_data_request();
        if cmd::send(CMD_SETMODE, &[MODE_DATA], INT_ACK, ACK_WAIT) != cmd::MATCHED {
            return code::NO_ANSWER;
        }
        code::OK
    }

    /// Save `I_MASK` (once per read) and keep only VBlank while the drive is polled, as psx-io's
    /// reader does: a CD-ROM IRQ with no handler would otherwise storm.
    #[inline(never)]
    pub fn enter_mask() {
        let s = st();
        if s.mask_saved == 0 {
            s.saved_mask = r32(I_MASK);
            s.mask_saved = 1;
        }
        w32(I_MASK, 1);
    }

    /// Give `I_MASK` back.
    pub fn release() {
        cmd::ack_all();
        let s = st();
        if s.mask_saved != 0 {
            w32(I_MASK, s.saved_mask);
            s.mask_saved = 0;
        }
    }

    /// How many `unit`s `rest` holds (fewer than 100 on any disc), taken out of `rest`, in BCD.
    /// Counted by subtraction: under 100 rounds, once per read start, and no divide code.
    #[inline(never)]
    fn take_bcd(rest: &mut u32, unit: u32) -> u8 {
        let mut n = 0;
        while *rest >= unit {
            *rest -= unit;
            n += 1;
            if n & 0xf == 10 {
                n += 6;
            }
        }
        n as u8
    }

    /// SetLoc, SeekL and its completion, SetMode, ReadN: the start the BIOS uses.
    #[inline(never)]
    pub fn start(lba: u32, implicit_seek: bool) -> bool {
        let mut abs = if lba & abi::READ_ABSOLUTE != 0 {
            lba & !abi::READ_ABSOLUTE
        } else {
            lba + st().lba_offset
        } + 150;
        let msf = [
            take_bcd(&mut abs, 75 * 60),
            take_bcd(&mut abs, 75),
            take_bcd(&mut abs, 1),
        ];
        if cmd::send(CMD_SETLOC, &msf, INT_ACK, ACK_WAIT) != cmd::MATCHED {
            return false;
        }
        if !implicit_seek {
            if cmd::send(CMD_SEEKL, &[], INT_ACK, ACK_WAIT) != cmd::MATCHED {
                return false;
            }
            match cmd::wait(INT_COMPLETE, FIRST_SECTOR_WAIT) {
                cmd::MATCHED => cmd::ack(INT_COMPLETE),
                cmd::CD_ERROR => {
                    cmd::drain();
                    cmd::ack_all();
                    return false;
                }
                _ => return false,
            }
            // The BIOS re-sends SetMode between the seek and ReadN; so does this.
            if cmd::send(CMD_SETMODE, &[MODE_DATA], INT_ACK, ACK_WAIT) != cmd::MATCHED {
                return false;
            }
        }
        if cmd::send(CMD_READN, &[], INT_ACK, ACK_WAIT) != cmd::MATCHED {
            return false;
        }
        cmd::set_irq_enable(0x1f);
        st().first_sector = 1;
        true
    }

    /// Wait for the next sector and pop it (`hash` as for [`pop`]).
    #[inline(never)]
    pub fn sector(dst: *mut u32, hash: *mut u32) -> bool {
        let s = st();
        let limit = if s.first_sector != 0 {
            FIRST_SECTOR_WAIT
        } else {
            NEXT_SECTOR_WAIT
        };
        s.first_sector = 0;
        if cmd::wait(INT_DATA_READY, limit) != cmd::MATCHED || !pop(dst, hash) {
            cmd::drain();
            cmd::ack_all();
            return false;
        }
        cmd::drain();
        cmd::ack(INT_DATA_READY);
        true
    }

    /// Mute the SPU's CD input before a data read that follows music.
    #[inline(never)]
    pub fn music_yield() {
        w16(SPU_CD_VOLUME, 0);
        w16(SPU_CD_VOLUME + 2, 0);
        w16(SPU_CONTROL, r16(SPU_CONTROL) & !1);
    }
}

// ---------------------------------------------------------------------------------------------
// read: the loop with retries.

mod read {
    use super::*;

    pub type Hook = Option<extern "C" fn(u32)>;

    /// Read `sectors` from member-relative `lba` into `dst` (null: discard), `CHUNK_SECTORS` per
    /// seek-first start, each chunk retried from a fresh start up to `DEFAULT_ATTEMPTS` times. The
    /// word FNV of everything read comes back in `hash`. Ends with an acknowledged Pause whose
    /// proof is owed to the next use of the drive (`drive::stop`).
    #[inline(never)]
    pub fn run(lba: u32, sectors: u32, dst: *mut u32, hook: Hook, hash: &mut u32) -> u32 {
        let r = drive::prepare();
        if r != code::OK {
            drive::release();
            return r;
        }
        let mut done = 0;
        let mut h = fnv::BASIS;
        while done < sectors {
            let n = CHUNK_SECTORS.min(sectors - done);
            let mut attempt = 0;
            loop {
                let mut ch = h;
                let mut k = 0;
                let mut ok = drive::start(lba + done, false);
                while ok && k < n {
                    let at = if dst.is_null() {
                        dst
                    } else {
                        dst.wrapping_add(((done + k) * SECTOR_WORDS) as usize)
                    };
                    ok = drive::sector(at, &mut ch);
                    if ok {
                        if let Some(f) = hook {
                            f(done + k);
                        }
                        k += 1;
                    }
                }
                if ok {
                    h = ch;
                    break;
                }
                attempt += 1;
                // A fresh start for the retry: stop and prove it, then try the chunk again.
                let _ = drive::pause_proved();
                if attempt >= DEFAULT_ATTEMPTS {
                    drive::release();
                    return code::READ;
                }
            }
            done += n;
            if done < sectors {
                let _ = drive::stop();
            }
        }
        // The data is in: the Pause's proof waits for the next use of the drive. A Pause that
        // was not acknowledged is found there too (the drive is then still active).
        let _ = drive::stop();
        drive::release();
        *hash = h;
        code::OK
    }
}

// ---------------------------------------------------------------------------------------------
// switch.

mod switch {
    use super::*;

    /// The rows of scene `want` (or of image `want` with `by_image`) from the caller's RAM copy of
    /// the table. On success `img` holds the image index.
    #[inline(never)]
    fn rows(want: u32, by_image: u32, table: *const u32) -> u32 {
        let s = st();
        // Neither a switch nor a load while a stream client holds the drive.
        if s.client != 0 {
            return code::CLIENT;
        }
        // The table is the caller's: read once at boot into its own RAM (psx-core's
        // `read_table`), so a switch never seeks for it.
        if table.is_null() {
            return code::TABLE_MAGIC;
        }
        // SAFETY (every read below): the caller's copy of the table, word-aligned, holding the
        // header and every row it counts; row indices are checked against the header first.
        let word = |i: u32| unsafe { core::ptr::read_volatile(table.add(i as usize)) };
        let head = word(2);
        if word(0) != table::MAGIC[0]
            || word(1) != table::MAGIC[1]
            || head & 0xffff != table::VERSION
        {
            return code::TABLE_MAGIC;
        }
        let scenes = word(3) & 0xffff;
        if scenes > table::MAX_ROWS {
            return code::NO_ROW;
        }
        if s.carry_pending != 0 {
            // The boot program (image 0) is running since the BIOS: the overlays its exe carries
            // are in RAM too.
            s.carry_pending = 0;
            // Image row 0's word 5, inside the copy (every table has image 0).
            s.mask |=
                word(table::HEADER_WORDS + table::ROW_WORDS * scenes + table::I_EVICTS as u32);
        }
        let image = if by_image != 0 {
            want
        } else {
            // Scene rows are looked up by their id: a compact copy keeps only the scenes the
            // program reaches, in id order.
            let mut i = 0;
            loop {
                if i == scenes {
                    return code::NO_ROW;
                }
                let at = table::HEADER_WORDS + table::ROW_WORDS * i;
                // Row `i` of `scenes`, inside the copy. Only its first two words are used: id,
                // image and flags, then the entry PC.
                if word(at) & 0xffff == want {
                    // SAFETY: as above.
                    unsafe { copy_words(s.scene.as_mut_ptr(), table.add(at as usize), 2) };
                    break;
                }
                i += 1;
            }
            (s.scene[0] >> 16) & 0xff
        };
        if image >= head >> 16 || image >= table::MAX_IMAGES || scenes + image >= table::MAX_ROWS {
            return code::NO_ROW;
        }
        unsafe {
            copy_words(
                s.image.as_mut_ptr(),
                table.add((table::HEADER_WORDS + table::ROW_WORDS * (scenes + image)) as usize),
                8,
            )
        };
        s.img = image;
        code::OK
    }

    /// Copy `n` words.
    ///
    /// # Safety
    /// Both ranges valid for `n` words.
    #[inline(never)]
    pub unsafe fn copy_words(dst: *mut u32, src: *const u32, n: u32) {
        let mut i = 0;
        while i < n as usize {
            // SAFETY: as the caller vouches.
            unsafe { core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i))) };
            i += 1;
        }
    }

    /// The image row's load range is whole sectors inside the program area.
    fn bounds_ok() -> bool {
        let im = &st().image;
        let (addr, size) = (im[table::I_ADDR], im[table::I_SIZE]);
        let end = addr.wrapping_add(size);
        addr >= 0x8001_0000 && size % 2048 == 0 && end <= abi::PAYLOAD_END && end >= addr
    }

    /// An overlay may be loaded now: it is one, its owner program is the one running, its range
    /// is sane and does not hold `caller` (the code that asked).
    fn overlay_ok(caller: u32) -> u32 {
        let s = st();
        let im = &s.image;
        let k = im[table::I_KIND];
        if k & 0xff != table::KIND_OVERLAY || !bounds_ok() {
            return code::BOUNDS;
        }
        if s.mask & (1 << ((k >> 16) & 0x1f)) == 0 {
            return code::NOT_LOADED;
        }
        if caller.wrapping_sub(im[table::I_ADDR]) < im[table::I_SIZE] {
            return code::BOUNDS;
        }
        code::OK
    }

    /// Read the image in the state's rows into its range unless its bit says it is there, check
    /// it, flush the I-cache and update the mask. The evicts word is applied after the image's
    /// own bit: a VOLATILE overlay (its owner also uses its range) lists itself, so its bit is
    /// never kept and every load reads it.
    #[inline(never)]
    fn load_overlay() -> u32 {
        let s = st();
        let bit = 1 << s.img;
        if s.mask & bit != 0 {
            return code::OK;
        }
        let r = load_into(0);
        if r != code::OK {
            return r;
        }
        dma_settle();
        flush_icache();
        s.mask = (s.mask | bit) & !s.image[table::I_EVICTS];
        code::OK
    }

    /// Read the image in the state's rows to its load address, `skip` sectors past its LBA (1 for
    /// a program's header), and check its checksum, the whole load retried up to the core-wide
    /// attempts.
    #[inline(never)]
    fn load_into(skip: u32) -> u32 {
        let im = st().image;
        let mut tries = 0;
        loop {
            let mut h = 0;
            let r = read::run(
                im[table::I_LBA] + skip,
                im[table::I_SIZE] / 2048,
                im[table::I_ADDR] as *mut u32,
                None,
                &mut h,
            );
            if r != code::OK {
                return r;
            }
            if h == im[table::I_FNV] {
                return code::OK;
            }
            tries += 1;
            if tries >= DEFAULT_ATTEMPTS {
                return code::PAYLOAD_FNV;
            }
        }
    }

    /// `psx_core_load_image`: load an overlay and return to the caller.
    #[inline(never)]
    pub fn load_image(image: u32, table: *const u32, caller: u32) -> u32 {
        let mut r = rows(image, 1, table);
        if r == code::OK {
            r = overlay_ok(caller);
        }
        if r == code::OK {
            r = load_overlay();
        }
        r
    }

    /// Let the DMA channels finish, aborting one that will not within the command wait.
    #[inline(never)]
    fn dma_settle() {
        let mut ch = DMA_CHCR0;
        while ch < DMA_CHCR0 + 7 * 0x10 {
            // About 2 ms at the slowest bus: a channel still busy then is stopped.
            let mut spins = 0x1_0000u32;
            while r32(ch) & (1 << 24) != 0 {
                spins -= 1;
                if spins == 0 {
                    w32(ch, r32(ch) & !(1 << 24));
                    break;
                }
            }
            ch += 0x10;
        }
    }

    /// Mask everything, let the DMA channels finish, silence the voices. No GP1(00h): the
    /// outgoing frame stays on screen.
    #[inline(never)]
    fn quiesce() {
        // SAFETY: SR is COP0 12; interrupts off, coprocessor 2 kept usable.
        unsafe {
            core::arch::asm!(".set noat", "mtc0 {0}, $12", "nop", "nop", ".set at", in(reg) 0x4000_0000u32)
        };
        w32(I_MASK, 0);
        w32(I_STAT, 0);
        dma_settle();
        w32(DICR, 0);
        w32(DPCR, 0x0765_4321);
        // Key off all 24 voices: two 16-bit registers.
        w16(SPU_KEY_OFF, 0xffff);
        w16(SPU_KEY_OFF + 2, 0x00ff);
    }

    /// The checks of a switch, the rows read among them; nothing changed when it refuses.
    #[inline(never)]
    fn accept(id: u32, args: *const [u32; 4], table: *const u32) -> u32 {
        let s = st();
        let r = rows(id, 0, table);
        if r != code::OK {
            return r;
        }
        let k = s.image[table::I_KIND];
        if k & 0xff == table::KIND_PROGRAM {
            if s.scene[0] & table::SCENE_START != 0 {
                if s.image[table::I_CORE_HASH] != s.hash {
                    return code::CORE_HASH;
                }
                if !bounds_ok() {
                    return code::BOUNDS;
                }
            } else if s.mask & (1 << s.img) == 0 {
                return code::NOT_LOADED;
            }
        } else {
            // The switch never returns, so the caller's own range may be overwritten.
            let r = overlay_ok(0);
            if r != code::OK {
                return r;
            }
        }
        if !args.is_null() {
            // SAFETY: the caller's four words; copied before anything can overwrite them.
            s.args = unsafe { core::ptr::read_volatile(args) };
        } else {
            s.args = [0; 4];
        }
        code::OK
    }

    /// The whole switch, from the outgoing scene's call.
    #[inline(never)]
    pub fn begin(id: u32, args: *const [u32; 4], table: *const u32) -> u32 {
        let r = accept(id, args, table);
        if r != code::OK {
            return r;
        }
        let s = st();
        if s.image[table::I_KIND] & 0xff == table::KIND_PROGRAM
            && s.scene[0] & table::SCENE_START != 0
        {
            // A program switch. The hook is the outgoing program's code, about to be overwritten.
            s.idle = None;
            quiesce();
            // SAFETY: `land` never returns, takes no arguments and uses only the new stack.
            unsafe {
                core::arch::asm!(
                    ".set noat",
                    "move $sp, {sp}",
                    "j {land}",
                    "nop",
                    sp = in(reg) abi::STACK_INIT,
                    land = sym land,
                    options(noreturn)
                )
            }
        }
        // An in-program entry: an overlay (read first unless it is in RAM) or a scene of the
        // program that is running. A failed load has overwritten RAM: no way back.
        let r = load_overlay();
        if r != code::OK {
            fatal_screen(r);
        }
        enter(block::FLAG_IN_PROGRAM)
    }

    /// Write the block for the scene in the state's rows and jump to its entry.
    fn enter(flag: u32) -> ! {
        let s = st();
        let id = s.scene[0] & 0xffff;
        let previous = s.current;
        s.current = id;
        s.switches += 1;
        write_block(id, previous, s.flags | flag);
        // SAFETY: the scene's code is in place, checked and the I-cache flushed; the registers
        // are the documented entry state of a scene (psx-scene-abi).
        unsafe {
            core::arch::asm!(
                ".set noat",
                "move $gp, $zero",
                "move $sp, {sp}",
                "move $fp, $zero",
                "jr {pc}",
                "nop",
                sp = in(reg) abi::STACK_INIT,
                pc = in(reg) s.scene[table::S_ENTRY],
                in("$4") abi::PSXD,
                in("$5") s.lba_offset,
                in("$6") s.cdda_base,
                in("$7") block::ADDR,
                options(noreturn)
            )
        }
    }

    /// Load a program, check, hand over. Runs on the switch stack.
    #[inline(never)]
    extern "C" fn land() -> ! {
        let s = st();
        let mut r = load_into(1);
        if r == code::OK {
            // A program is entered with the drive proven stopped (CORE-ABI 3.3).
            r = drive::settle();
        }
        if r != code::OK {
            fatal_screen(r);
        }
        flush_icache();
        // The incoming program's disc bases: its member's, relative to where the table was read.
        s.lba_offset += s.image[table::I_BASE];
        s.cdda_base += s.image[table::I_KIND] >> 24;
        s.flags = if s.lba_offset != 0 {
            block::FLAG_NESTED
        } else {
            0
        };
        // The program and the overlays its own exe carries (its row's word 5).
        s.mask = (1 << s.img) | s.image[table::I_EVICTS];
        enter(block::FLAG_CORE_ENTRY)
    }
}

// ---------------------------------------------------------------------------------------------
// The handoff block.

#[inline(never)]
fn write_block(current: u32, previous: u32, flags: u32) {
    let s = st();
    let b = block::ADDR;
    s.scenes = current | (previous << 16);
    s.block_flags = flags;
    w32(b + block::OFF_NOT_MAGIC, 0);
    w32(b + block::OFF_MAGIC, block::MAGIC);
    w32(
        b + block::OFF_VERSION,
        block::VERSION | (block::BYTES << 16),
    );
    // SAFETY: the ten State words from `hash` mirror the block's 0x08..0x30 (repr(C), in order).
    unsafe {
        switch::copy_words(
            (b + block::OFF_CORE_HASH) as *mut u32,
            core::ptr::addr_of!(s.hash),
            10,
        )
    };
    w32(b + block::OFF_MASK, s.mask);
    w32(b + block::OFF_NOT_MAGIC, !block::MAGIC);
}

fn block_valid() -> bool {
    r32(block::ADDR + block::OFF_MAGIC) == block::MAGIC
        && r32(block::ADDR + block::OFF_NOT_MAGIC) == !block::MAGIC
}

// ---------------------------------------------------------------------------------------------
// I-cache.

/// Invalidate every I-cache line. Runs uncached (KSEG1), interrupts already off, DMA idle.
#[inline(never)]
fn flush_icache() {
    // SAFETY: the sequence of psx-rt's `__psx_rt_flush_i_cache`: isolate the cache, zero every
    // line tag, restore; only caller-saved registers, no stack.
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            "la    $t0, 1f",
            "lui   $t1, 0xA000",
            "or    $t0, $t0, $t1",
            "jr    $t0",
            "nop",
            "1:",
            "mfc0  $t2, $12",
            "lui   $t3, 0xFFFE",
            "mtc0  $zero, $12",
            "li    $t4, 0x00000804",
            "sw    $t4, 0x130($t3)",
            "lui   $t4, 0x0001",
            "mtc0  $t4, $12",
            "nop",
            "nop",
            "move  $t5, $zero",
            "li    $t6, 0x1000",
            "2:",
            "addiu $t5, $t5, 16",
            "bne   $t5, $t6, 2b",
            "sw    $zero, -16($t5)",
            "mtc0  $zero, $12",
            "nop",
            "nop",
            "li    $t4, 0x0001E988",
            "sw    $t4, 0x130($t3)",
            "mtc0  $t2, $12",
            "nop",
            ".set reorder",
            out("$8") _, out("$9") _, out("$10") _, out("$11") _, out("$12") _, out("$13") _, out("$14") _,
        )
    }
}

// ---------------------------------------------------------------------------------------------
// fatal.

#[inline(never)]
fn fatal_screen(stage: u32) -> ! {
    // SAFETY: interrupts off before the screen is touched.
    unsafe { core::arch::asm!("mtc0 $zero, $12", "nop", "nop") };
    st().fatal = stage;
    psx_core::fatal::paint(stage);
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------------------------
// Entries (jump table targets).

#[no_mangle]
extern "C" fn __psx_core_attach_impl(a0: u32, a1: u32, a2: u32, hash: u32) {
    let s = st();
    if s.hash != 0 {
        // A core entry: the switch wrote the block; it must still be there.
        if !block_valid() {
            fatal_screen(code::BAD_BLOCK);
        }
        return;
    }
    s.hash = hash;
    // The boot program is image 0 of its table; what its exe carries is added at the first table.
    s.mask = 1;
    s.carry_pending = 1;
    if a0 == abi::PSXD {
        s.lba_offset = a1;
        s.cdda_base = a2;
        s.flags = if a1 != 0 { block::FLAG_NESTED } else { 0 };
    }
    write_block(0, block::NO_SCENE, s.flags);
}

#[no_mangle]
extern "C" fn __psx_core_switch_impl(scene: u32, args: *const [u32; 4], table: *const u32) -> u32 {
    switch::begin(scene, args, table)
}

#[no_mangle]
extern "C" fn __psx_core_load_image_impl(image: u32, table: *const u32) -> u32 {
    // The slot is a plain `j`, so `$ra` is still the caller's return address: the load refuses
    // to overwrite the code it would return to.
    let caller: u32;
    // SAFETY: a register move.
    unsafe {
        core::arch::asm!("move {0}, $ra", out(reg) caller, options(nomem, nostack, preserves_flags))
    };
    switch::load_image(image, table, caller)
}

#[no_mangle]
extern "C" fn __psx_core_read_impl(lba: u32, sectors: u32, dst: *mut u32, hook: read::Hook) -> u32 {
    if st().client != 0 {
        return code::CLIENT;
    }
    let mut h = 0;
    read::run(lba, sectors, dst, hook, &mut h)
}

#[no_mangle]
extern "C" fn __psx_core_pause_impl() -> u32 {
    drive::enter_mask();
    // The previous Pause (or whatever the drive does) is proven before this one is sent; this one
    // is only acknowledged, its proof owed to the next use of the drive.
    let mut r = drive::settle();
    if r == code::OK && drive::stop() != cmd::MATCHED {
        r = code::NO_ANSWER;
    }
    drive::release();
    r
}

#[no_mangle]
extern "C" fn __psx_core_stream_open_impl(lba: u32, flags: u32) -> u32 {
    let r = drive::prepare();
    if r != code::OK {
        drive::release();
        return r;
    }
    // The core-wide retry policy, as a blocking read's: a start that fails is retried from a
    // proven Pause, three times in all.
    let mut attempt = 0;
    while attempt < DEFAULT_ATTEMPTS {
        if drive::start(lba, flags & abi::STREAM_IMPLICIT_SEEK != 0) {
            return code::OK;
        }
        let _ = drive::pause_proved();
        attempt += 1;
    }
    drive::release();
    code::READ
}

#[no_mangle]
extern "C" fn __psx_core_stream_sector_impl(dst: *mut u32) -> u32 {
    // No checksum: the stream client checks its own data, the shape of psx-io's reader.
    if drive::sector(dst, core::ptr::null_mut()) {
        code::OK
    } else {
        code::READ
    }
}

/// The CD command slot for a game's own driver (HL's XA player): op 0 only, a command and the
/// core's bounded wait for its acknowledge. `b` = n | command << 8, the n parameter bytes (16 at
/// most) at `a`; the response (16 bytes at most) copied to `c`, or discarded when `c` is null.
/// Returns the outcome (0 acknowledged, 1 INT5, 2 timeout) | length << 8. The register-level ops
/// (1 to 6: no-wait command, flag, acknowledge, mixer, IRQ enables, response) hold no core state
/// and live in the program (psx-io's `core-cd`); any op but 0 returns `u32::MAX`.
#[no_mangle]
extern "C" fn __psx_core_cd_impl(op: u32, a: u32, b: u32, c: *mut u8) -> u32 {
    if op != 0 {
        return u32::MAX;
    }
    // The drive is proven stopped before any command the core sends, this one included (with
    // the caller's `I_MASK`, as the command itself: no CD-ROM IRQ handler is installed).
    let _ = drive::settle();
    let params = if a == 0 || b & 0xff == 0 {
        &[][..]
    } else {
        // SAFETY: the caller's `b` parameter bytes at `a`.
        unsafe { core::slice::from_raw_parts(a as *const u8, (b & 0xff).min(16) as usize) }
    };
    cmd::send_collect((b >> 8) as u8, params, c)
}

#[no_mangle]
extern "C" fn __psx_core_set_idle_hook_impl(hook: Option<extern "C" fn()>) {
    st().idle = hook;
}

#[no_mangle]
extern "C" fn __psx_core_client_attach_impl() -> u32 {
    let s = st();
    let held = s.client;
    s.client = 1;
    if held != 0 {
        code::CLIENT
    } else {
        code::OK
    }
}

#[no_mangle]
extern "C" fn __psx_core_client_detach_impl() {
    st().client = 0;
}

#[no_mangle]
extern "C" fn __psx_core_fatal_impl(stage: u32) -> ! {
    fatal_screen(stage)
}

#[no_mangle]
extern "C" fn __psx_core_music_yield_impl() {
    drive::music_yield()
}

core::arch::global_asm!(
    ".section .text.__psx_core_table,\"ax\",@progbits",
    ".globl __psx_core_table",
    ".set noreorder",
    "__psx_core_table:",
    "j __psx_core_attach_impl",
    "nop",
    "j __psx_core_switch_impl",
    "nop",
    "j __psx_core_read_impl",
    "nop",
    "j __psx_core_pause_impl",
    "nop",
    "j __psx_core_client_attach_impl",
    "nop",
    "j __psx_core_client_detach_impl",
    "nop",
    "j __psx_core_fatal_impl",
    "nop",
    "j __psx_core_music_yield_impl",
    "nop",
    "j __psx_core_stream_open_impl",
    "nop",
    "j __psx_core_stream_sector_impl",
    "nop",
    "j __psx_core_cd_impl",
    "nop",
    "j __psx_core_set_idle_hook_impl",
    "nop",
    "j __psx_core_load_image_impl",
    "nop",
    ".set reorder",
);

#[cfg(feature = "alt")]
#[used]
#[no_mangle]
static __PSX_CORE_ALT: u32 = 0xa17c_0de5;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fatal_screen(0xff)
}
