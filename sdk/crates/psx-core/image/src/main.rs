// SPDX-License-Identifier: GPL-2.0-or-later
//! The scene core's image: everything that runs from the window 0x801FDF00..0x801FFF00.
//!
//! Built alone (`image.ld`), flattened to `psx-core.bin` and embedded byte for byte in every
//! program on the core. Sections, in the order a size report lists them:
//! - `cmd`: the CD command layer (send and wait, response FIFO, bounded drain, acknowledge);
//! - `drive`: the polled reader and its rules: never assume an idle drive (settle on first use),
//!   seek-first starts, PIO sector pops (no CD DMA), Pause and never Stop, a Pause proven stopped
//!   and resent when refused mid-seek, the CD input muted before a read after music;
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
const CACHE_CONTROL: u32 = 0xfffe_0130;

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

/// Written by a cold attach; a core entry that does not find it has no live core.
const ALIVE: u32 = 0xc0de_5ce1;
/// [`State::keep`] when no table row is being read. A kept row is stored as its word index + 1,
/// so the statics start all zero and sit in `.bss`, outside the hashed image.
const NO_KEEP: u32 = 0;

#[repr(C)]
struct State {
    alive: u32,
    hash: u32,
    lba_offset: u32,
    cdda_base: u32,
    current: u32,
    flags: u32,
    switches: u32,
    /// A stream client holds the drive.
    client: u32,
    /// The drive was settled once (a drive is never assumed idle).
    prepared: u32,
    /// `I_MASK` as the caller had it while a polled read runs.
    saved_mask: u32,
    /// First response byte of the last command that matched.
    last_status: u32,
    /// Last failure: `cause << 24 | status << 16 | command << 8 | flag`.
    diag: u32,
    /// The running read has not delivered a sector yet.
    first_sector: u32,
    /// A stream sector: popped with no checksum (the stream client checks its own data).
    no_hash: u32,
    /// `I_MASK` is saved in `saved_mask` (a read or a stream is open).
    mask_saved: u32,
    /// Table read: word index + 1 of the row kept from the sector, or [`NO_KEEP`].
    keep: u32,
    /// The program's idle hook, called from every spin of every wait.
    idle: Option<extern "C" fn()>,
    /// Stage of the last fatal screen.
    fatal: u32,
    /// Arguments of the switch in flight.
    args: [u32; 4],
    /// The row of the switch in flight; header words 0..4 of the table sector land in `head`.
    row: [u32; 16],
    head: [u32; 4],
}

static mut ST: State = State {
    alive: 0,
    hash: 0,
    lba_offset: 0,
    cdda_base: 0,
    current: 0,
    flags: 0,
    switches: 0,
    client: 0,
    prepared: 0,
    saved_mask: 0,
    last_status: 0,
    diag: 0,
    first_sector: 0,
    no_hash: 0,
    mask_saved: 0,
    keep: NO_KEEP,
    idle: None,
    fatal: 0,
    args: [0; 4],
    row: [0; 16],
    head: [0; 4],
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
    #[inline(always)]
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
                let status = r8(CD_CMD) as u32;
                let err = r8(CD_CMD) as u32;
                st().diag = (5 << 24) | (status << 16) | ((command as u32) << 8) | err;
                drain();
                ack_all();
            }
            _ => st().diag = (0xff << 24) | ((command as u32) << 8) | flag() as u32,
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
    /// word into `hash`, and keep table header words 0..4 in the state. `false` when the FIFO
    /// never filled (the request is dropped, so a late sector cannot land in the next read).
    #[inline(never)]
    #[optimize(speed)]
    pub fn pop(dst: *mut u32, hash: &mut u32) -> bool {
        w8(CD_REQ, 0x80);
        let mut e = Elapsed::start();
        while !cmd::data_ready() {
            if e.step(FIFO_WAIT) {
                cmd::clear_data_request();
                return false;
            }
        }
        let mut h = *hash;
        let keep = st().keep;
        if keep == NO_KEEP && !dst.is_null() && st().no_hash != 0 {
            // A stream client's sector: the plain PIO pop, as psx-io's reader does it.
            let mut at = dst;
            let end = dst.wrapping_add(SECTOR_WORDS as usize);
            while at != end {
                let b0 = r8(CD_PARAM) as u32;
                let b1 = r8(CD_PARAM) as u32;
                let b2 = r8(CD_PARAM) as u32;
                let b3 = r8(CD_PARAM) as u32;
                // SAFETY: the caller's destination covers a whole sector.
                unsafe { core::ptr::write_volatile(at, (b3 << 24) | (b2 << 16) | (b1 << 8) | b0) };
                at = at.wrapping_add(1);
            }
            return true;
        }
        if keep == NO_KEEP && !dst.is_null() {
            // The payload path, hot in every load and every streamed sector: no per-word tests.
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
                at = at.wrapping_add(1);
            }
            *hash = h;
            return true;
        }
        let mut i = 0;
        while i < SECTOR_WORDS {
            let b0 = r8(CD_PARAM) as u32;
            let b1 = r8(CD_PARAM) as u32;
            let b2 = r8(CD_PARAM) as u32;
            let b3 = r8(CD_PARAM) as u32;
            let word = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0;
            h = fnv::step(h, word);
            if !dst.is_null() {
                // SAFETY: the caller's destination covers a whole sector.
                unsafe { core::ptr::write_volatile(dst.add(i as usize), word) };
            }
            if keep != NO_KEEP {
                if i < 4 {
                    st().head[i as usize] = word;
                }
                if i.wrapping_sub(keep - 1) < 16 {
                    st().row[(i + 1 - keep) as usize] = word;
                }
            }
            i += 1;
        }
        *hash = h;
        true
    }

    /// Clear an IRQ nobody waited for. A stale DataReady has its sector drained first, or the
    /// FIFO stays occupied and later reads misalign.
    #[inline(never)]
    pub fn ack_unexpected(f: u8) {
        if f == INT_DATA_READY {
            let mut h = 0;
            let _ = pop(core::ptr::null_mut(), &mut h);
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

    /// The acknowledge-only stop between chunks: Pause until the drive takes it (once more
    /// after a refusal mid-seek), without waiting for it to stop.
    #[inline(never)]
    pub fn stop() {
        let mut r = cmd::send(CMD_PAUSE, &[], INT_ACK, ACK_WAIT);
        if r == cmd::CD_ERROR {
            wait_seek_end();
            r = cmd::send(CMD_PAUSE, &[], INT_ACK, ACK_WAIT);
        }
        if r == cmd::MATCHED {
            cmd::drain();
            cmd::ack(INT_COMPLETE);
        }
        cmd::ack_all();
    }

    /// Take the controller for polled reads: the CD source out of `I_MASK` until [`release`],
    /// controller IRQs on, and on the first use a settle of whatever the drive is doing.
    #[inline(never)]
    pub fn prepare() -> u32 {
        let s = st();
        enter_mask();
        w32(I_STAT, !CDROM_IRQ_BIT);
        cmd::set_irq_enable(0x1f);
        cmd::ack_all();
        if s.prepared == 0 {
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
                // No answer: SetMode below fails on its own bound and says so.
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
            cmd::ack_all();
            s.prepared = 1;
        }
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

    #[inline(never)]
    fn bcd(v: u32) -> u8 {
        let tens = v / 10;
        ((tens << 4) | (v - tens * 10)) as u8
    }

    /// SetLoc, SeekL and its completion, SetMode, ReadN: the start the BIOS uses.
    #[inline(never)]
    pub fn start(lba: u32, implicit_seek: bool) -> bool {
        let abs = if lba & abi::READ_ABSOLUTE != 0 {
            lba & !abi::READ_ABSOLUTE
        } else {
            lba + st().lba_offset
        } + 150;
        let minutes = abs / (75 * 60);
        let rest = abs - minutes * (75 * 60);
        let seconds = rest / 75;
        let msf = [bcd(minutes), bcd(seconds), bcd(rest - seconds * 75)];
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

    /// Wait for the next sector and pop it.
    #[inline(never)]
    pub fn sector(dst: *mut u32, hash: &mut u32) -> bool {
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
    /// seek-first start, each chunk retried from a fresh start up to `attempts` times. The word
    /// FNV of everything read comes back in `hash`. Ends with a proven Pause.
    #[inline(never)]
    pub fn run(
        lba: u32,
        sectors: u32,
        dst: *mut u32,
        hook: Hook,
        attempts: u32,
        hash: &mut u32,
    ) -> u32 {
        let r = drive::prepare();
        if r != code::OK {
            drive::release();
            return r;
        }
        let mut done = 0;
        let mut h = *hash;
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
                if attempt >= attempts {
                    drive::release();
                    return code::READ;
                }
            }
            done += n;
            if done < sectors {
                drive::stop();
            }
        }
        // A table row read ends with the acknowledge-only stop: the switch either reads the
        // payload next (one proven Pause at its end, not two) or proves the Pause on refusal.
        let r = if st().keep != NO_KEEP {
            drive::stop();
            code::OK
        } else {
            drive::pause_proved()
        };
        drive::release();
        *hash = h;
        r
    }
}

// ---------------------------------------------------------------------------------------------
// switch.

mod switch {
    use super::*;

    /// Read row `id` of the table into the state: its one sector, popped word by word, the
    /// header's first words and the row's 16 kept, the rest dropped.
    #[inline(never)]
    fn row(id: u32) -> u32 {
        let s = st();
        if id >= table::MAX_ROWS {
            return code::NO_ROW;
        }
        s.keep = table::row_word(id) + 1;
        let mut h = 0;
        let r = read::run(
            table::LBA,
            1,
            core::ptr::null_mut(),
            None,
            DEFAULT_ATTEMPTS,
            &mut h,
        );
        s.keep = NO_KEEP;
        if r != code::OK {
            return r;
        }
        let head = s.head[table::W_HEAD_VERSION];
        if s.head[0] != table::MAGIC[0]
            || s.head[1] != table::MAGIC[1]
            || head & 0xffff != table::VERSION
        {
            return code::TABLE_MAGIC;
        }
        if id >= head >> 16 || s.row[table::W_ID_KIND] & 0xffff != id {
            return code::NO_ROW;
        }
        code::OK
    }

    /// Mask everything, let the DMA channels finish (abort one that will not), silence the
    /// voices. No GP1(00h): the outgoing frame stays on screen.
    #[inline(never)]
    fn quiesce() {
        // SAFETY: SR is COP0 12; interrupts off, coprocessor 2 kept usable.
        unsafe {
            core::arch::asm!(".set noat", "mtc0 {0}, $12", "nop", "nop", ".set at", in(reg) 0x4000_0000u32)
        };
        w32(I_MASK, 0);
        w32(I_STAT, 0);
        let mut ch = 0;
        while ch < 7 {
            let chcr = DMA_CHCR0 + ch * 0x10;
            let mut e = Elapsed::start();
            while r32(chcr) & (1 << 24) != 0 {
                if e.step(ACK_WAIT) {
                    w32(chcr, r32(chcr) & !(1 << 24));
                    break;
                }
            }
            ch += 1;
        }
        w32(DICR, 0);
        w32(DPCR, 0x0765_4321);
        // Key off all 24 voices: two 16-bit registers.
        w16(SPU_KEY_OFF, 0xffff);
        w16(SPU_KEY_OFF + 2, 0x00ff);
    }

    /// The whole switch, from the outgoing program's call.
    #[inline(never)]
    pub fn begin(id: u32, args: *const [u32; 4]) -> u32 {
        let r = accept(id, args);
        if r != code::OK && r != code::CLIENT {
            // Refused after the row read: the outgoing program carries on with a proven Pause.
            drive::enter_mask();
            let _ = drive::pause_proved();
            drive::release();
            return r;
        }
        if r != code::OK {
            return r;
        }
        // The hook is the outgoing program's code, about to be overwritten.
        st().idle = None;
        quiesce();
        // From here on the outgoing program is gone: run at the top of its stack reserve.
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

    /// The checks of a switch, the row read among them; nothing changed when it refuses.
    #[inline(never)]
    fn accept(id: u32, args: *const [u32; 4]) -> u32 {
        let s = st();
        if s.client != 0 {
            return code::CLIENT;
        }
        let r = row(id);
        if r != code::OK {
            return r;
        }
        let row = &s.row;
        if row[table::W_CORE_HASH] != s.hash {
            return code::CORE_HASH;
        }
        let (t_addr, t_size) = (row[table::W_T_ADDR], row[table::W_T_SIZE]);
        let kind = (row[table::W_ID_KIND] >> 16) & 0xff;
        if kind != table::KIND_PROGRAM
            || t_addr < 0x8001_0000
            || t_size % 2048 != 0
            || t_addr.wrapping_add(t_size) > abi::PAYLOAD_END
            || t_addr.wrapping_add(t_size) < t_addr
        {
            return code::BOUNDS;
        }
        if !args.is_null() {
            // SAFETY: the caller's four words; copied before anything can overwrite them.
            s.args = unsafe { core::ptr::read_volatile(args) };
        } else {
            s.args = [0; 4];
        }
        code::OK
    }

    /// Load, check, hand over. Runs on the switch stack.
    #[inline(never)]
    extern "C" fn land() -> ! {
        let s = st();
        let row = s.row;
        let (t_addr, t_size) = (row[table::W_T_ADDR], row[table::W_T_SIZE]);
        // Retries are core-wide: the row's attempts byte is reserved.
        let attempts = DEFAULT_ATTEMPTS;
        let mut tries = 0;
        loop {
            let mut h = fnv::BASIS;
            let r = read::run(
                row[table::W_LBA] + 1,
                t_size / 2048,
                t_addr as *mut u32,
                None,
                attempts,
                &mut h,
            );
            if r != code::OK {
                fatal_screen(r);
            }
            if h == row[table::W_FNV] {
                break;
            }
            tries += 1;
            if tries >= attempts {
                fatal_screen(code::PAYLOAD_FNV);
            }
        }
        flush_icache();
        // The incoming program's disc bases: its member's, relative to where the table was read.
        s.lba_offset += row[table::W_LBA_OFFSET];
        s.cdda_base += row[table::W_PARTNER_CDDA] >> 16;
        s.flags = if s.lba_offset != 0 {
            block::FLAG_NESTED
        } else {
            0
        };
        let id = row[table::W_ID_KIND] & 0xffff;
        let previous = s.current;
        s.current = id;
        s.switches += 1;
        write_block(id, previous, s.flags | block::FLAG_CORE_ENTRY);
        // SAFETY: the payload is in place, checked and the I-cache flushed; the registers are
        // the documented entry state of a scene (psx-scene-abi).
        unsafe {
            core::arch::asm!(
                ".set noat",
                "move $gp, $zero",
                "move $sp, {sp}",
                "move $fp, $zero",
                "jr {pc}",
                "nop",
                sp = in(reg) row[table::W_SP],
                pc = in(reg) row[table::W_PC],
                in("$4") abi::PSXD,
                in("$5") s.lba_offset,
                in("$6") s.cdda_base,
                in("$7") block::ADDR,
                options(noreturn)
            )
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The handoff block.

#[inline(never)]
fn write_block(current: u32, previous: u32, flags: u32) {
    let s = st();
    let b = block::ADDR;
    w32(b + block::OFF_NOT_MAGIC, 0);
    w32(b + block::OFF_MAGIC, block::MAGIC);
    w32(
        b + block::OFF_VERSION,
        block::VERSION | (block::BYTES << 16),
    );
    w32(b + block::OFF_CORE_HASH, s.hash);
    w32(b + block::OFF_LBA_OFFSET, s.lba_offset);
    w32(b + block::OFF_CDDA_BASE, s.cdda_base);
    w32(b + block::OFF_SCENES, current | (previous << 16));
    w32(b + block::OFF_FLAGS, flags);
    w32(b + block::OFF_SWITCHES, s.switches);
    let mut i = 0;
    while i < 4 {
        w32(b + block::OFF_ARGS + 4 * i, s.args[i as usize]);
        i += 1;
    }
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
    let _ = CACHE_CONTROL;
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
    if s.alive == ALIVE {
        // A core entry: the switch wrote the block; it must still be there.
        if !block_valid() {
            fatal_screen(code::BAD_BLOCK);
        }
        return;
    }
    s.alive = ALIVE;
    s.hash = hash;
    if a0 == abi::PSXD {
        s.lba_offset = a1;
        s.cdda_base = a2;
        s.flags = if a1 != 0 { block::FLAG_NESTED } else { 0 };
    }
    write_block(0, block::NO_SCENE, s.flags);
}

#[no_mangle]
extern "C" fn __psx_core_switch_impl(scene: u32, args: *const [u32; 4]) -> u32 {
    switch::begin(scene, args)
}

#[no_mangle]
extern "C" fn __psx_core_read_impl(lba: u32, sectors: u32, dst: *mut u32, hook: read::Hook) -> u32 {
    if st().client != 0 {
        return code::CLIENT;
    }
    let mut h = fnv::BASIS;
    read::run(lba, sectors, dst, hook, DEFAULT_ATTEMPTS, &mut h)
}

#[no_mangle]
extern "C" fn __psx_core_pause_impl() -> u32 {
    let r = if st().prepared == 0 {
        drive::prepare()
    } else {
        drive::enter_mask();
        code::OK
    };
    let r = if r == code::OK {
        drive::pause_proved()
    } else {
        r
    };
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
    let mut h = 0;
    let s = st();
    s.no_hash = 1;
    let ok = drive::sector(dst, &mut h);
    s.no_hash = 0;
    if ok {
        code::OK
    } else {
        code::READ
    }
}

/// The register-level CD slot for a game's own drivers (HL's XA player). `op`:
/// - 0 command: `b` = n | command << 8, the n parameter bytes at `a` sent, wait for the acknowledge, its
///   response (up to 16 bytes) copied to `c` when not null; returns the outcome (0 acknowledged,
///   1 INT5, 2 timeout) | length << 8;
/// - 1 command without waiting: parameters and command byte written, controller IRQs as they
///   were; the caller collects the answer with ops 2, 3 and 6;
/// - 2 read the controller's IRQ flag; 3 acknowledge flag bits `a` (and `I_STAT`'s CD bit);
/// - 4 audio mixer, `a` = L->L | L->R << 8 | R->R << 16 | R->L << 24, then apply;
/// - 5 set the controller's IRQ enables to `a`, returns the old ones;
/// - 6 pop the response FIFO (up to 16 bytes) into `c`, returns the length.
#[no_mangle]
extern "C" fn __psx_core_cd_impl(op: u32, a: u32, b: u32, c: *mut u8) -> u32 {
    match op {
        0 | 1 => {
            let params = if a == 0 || b & 0xff == 0 {
                &[][..]
            } else {
                // SAFETY: the caller's `b` parameter bytes at `a`.
                unsafe { core::slice::from_raw_parts(a as *const u8, (b & 0xff).min(16) as usize) }
            };
            if op == 0 {
                cmd::send_collect((b >> 8) as u8, params, c)
            } else {
                cmd::dispatch((b >> 8) as u8, params)
            }
        }
        2 => cmd::flag() as u32,
        3 => {
            cmd::ack(a as u8);
            0
        }
        4 => {
            cmd::index(2);
            w8(CD_PARAM, a as u8);
            w8(CD_REQ, (a >> 8) as u8);
            cmd::index(3);
            w8(CD_CMD, (a >> 16) as u8);
            w8(CD_PARAM, (a >> 24) as u8);
            w8(CD_REQ, 0x20);
            cmd::index(0);
            0
        }
        5 => {
            let old = cmd::irq_enable() as u32;
            cmd::set_irq_enable(a as u8);
            old
        }
        6 => cmd::collect(c),
        _ => u32::MAX,
    }
}

#[no_mangle]
extern "C" fn __psx_core_set_idle_hook_impl(hook: Option<extern "C" fn()>) {
    st().idle = hook;
}

#[no_mangle]
extern "C" fn __psx_core_client_attach_impl() -> u32 {
    let s = st();
    if s.client != 0 {
        return code::CLIENT;
    }
    s.client = 1;
    code::OK
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
