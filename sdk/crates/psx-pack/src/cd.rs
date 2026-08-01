// SPDX-License-Identifier: GPL-2.0-or-later
//! CD-ROM sector-read state machine: stream pack chunks straight off the disc.
//!
//! This is the hardware half the crate doc used to defer to the caller. It is
//! a faithful port of hl-psx's `cdstream.rs` `hw` module (itself a cleaned-up
//! second generation of the engine's `editor-playtest/src/cd_stream/hw.rs`),
//! which streams 96 maps through this exact command sequence on real silicon.
//! The command order, poll limits, and IRQ ack ordering are deliberately
//! identical; the comments that record silicon findings travel with the code.
//! What changed is packaging only: the module-level `static mut` state
//! (`CD_READ_PREPARED`, the bounce sector buffer) now lives inside
//! [`SectorReader`], and the pack-table scan reuses the crate's pure parsing
//! helpers ([`crate::parse_header`], [`crate::parse_entry_at`],
//! [`crate::entry_location`]) instead of ad-hoc pointer reads.
//!
//! Everything that touches MMIO is `cfg(target_arch = "mips")`; on the host
//! only the pure pieces (constants, BCD MSF math) compile, so `cargo test`
//! keeps covering them.
//!
//! # What `prepare()` does to the machine (read this before calling)
//!
//! [`SectorReader::prepare`] takes ownership of the CD controller and, more
//! intrusively, of the interrupt controller:
//!
//! * **`I_MASK` is rewritten to VBlank-only** (`irq::set_mask(1 << VBLANK)`).
//!   Every other IRQ source (CD-ROM, DMA, SPU, timers, pads...) stops reaching
//!   the CPU until the caller restores its own mask. The reader runs the CD
//!   controller purely by polling its flag register, so a CD-ROM CPU IRQ with
//!   no handler installed would otherwise be an unhandled-IRQ storm.
//! * The latched CD-ROM `I_STAT` bit is acked and all five controller-level
//!   IRQ enables are switched on (`0x1F`), then every pending controller IRQ
//!   is acked (`0x5F`: flags + parameter-FIFO reset bit).
//! * DMA channel 3 (CD-ROM) is enabled in `DPCR`.
//! * On the first `prepare()` of this reader, pending IRQs latched by earlier
//!   activity (e.g. the BIOS disc boot's file load) are drained. Do NOT send
//!   Pause before the first stream instead: on real BIOS boot paths some
//!   emulators have no active read command to pause and never acknowledge it.
//! * `Setmode(0x80)`: double speed, 2048-byte user data sectors.
//!
//! No caching happens at this layer. hl-psx's 512-entry table cache (skip
//! re-scanning the header on every chunk load) is a game-side optimization:
//! keep it in the game, keyed to its own pack, where the entry count and the
//! RAM budget are known.

#[allow(unused_imports)]
use crate::{entry_location, parse_entry_at, parse_header, PackEntry, ENTRY_BYTES, SECTOR_BYTES};

/// One CD sector's user data as DMA words (`SECTOR_BYTES / 4`).
pub const SECTOR_WORDS: usize = SECTOR_BYTES / 4;

/// Where `mkisopsx` / the editor's embedded Play place `WORLD.PAK`: the pack's
/// first sector, as an absolute data-track LBA. Mirrors
/// `psx_iso::WORLD_PACK_DEFAULT_START_LBA` (the writer reserves a fixed boot
/// area so runtime LBAs never depend on the boot EXE size); a host test in
/// this crate asserts the two constants stay equal.
pub const WORLD_PACK_DEFAULT_LBA: u32 = 1024;

// --- CD-ROM controller registers (behind the index register's low 2 bits) ---
#[cfg(target_arch = "mips")]
const CD_BASE: u32 = 0x1F80_1800;
#[cfg(target_arch = "mips")]
const CD_STATUS: u32 = CD_BASE;
#[cfg(target_arch = "mips")]
const CD_RESPONSE: u32 = CD_BASE + 1;
#[cfg(target_arch = "mips")]
const CD_PARAM: u32 = CD_BASE + 2;
/// Same port, read side: index-0 reads pop the data FIFO one byte at a time.
#[cfg(target_arch = "mips")]
const CD_DATA: u32 = CD_BASE + 2;
#[cfg(target_arch = "mips")]
const CD_IRQ: u32 = CD_BASE + 3;

#[cfg(target_arch = "mips")]
const STATUS_RESPONSE_FIFO_NOT_EMPTY: u8 = 1 << 5;
#[cfg(target_arch = "mips")]
const STATUS_PARAMETER_FIFO_NOT_FULL: u8 = 1 << 4;
#[cfg(target_arch = "mips")]
const STATUS_DATA_FIFO_NOT_EMPTY: u8 = 1 << 6;

#[cfg(target_arch = "mips")]
const IRQ_DATA_READY: u8 = 1;
#[cfg(target_arch = "mips")]
const IRQ_COMPLETE: u8 = 2;
#[cfg(target_arch = "mips")]
const IRQ_ACK: u8 = 3;
#[cfg(target_arch = "mips")]
const IRQ_DATA_END: u8 = 4;
#[cfg(target_arch = "mips")]
const IRQ_ERROR: u8 = 5;

#[cfg(target_arch = "mips")]
const CMD_SETLOC: u8 = 0x02;
#[cfg(target_arch = "mips")]
const CMD_READN: u8 = 0x06;
#[cfg(target_arch = "mips")]
const CMD_PAUSE: u8 = 0x09;
#[cfg(target_arch = "mips")]
const CMD_SETMODE: u8 = 0x0E;
#[cfg(target_arch = "mips")]
const CMD_SEEKL: u8 = 0x15;
#[cfg(target_arch = "mips")]
const CD_MODE_DOUBLE_SPEED_2048: u8 = 0x80;

// Poll limits, straight from hl-psx (tuned on silicon; DATA_POLL covers a
// worst-case seek at double speed).
#[cfg(target_arch = "mips")]
const ACK_POLL: u32 = 16_384;
#[cfg(target_arch = "mips")]
const PARAM_POLL: u32 = 16_384;
#[cfg(target_arch = "mips")]
const DATA_POLL: u32 = 4_000_000;
#[cfg(target_arch = "mips")]
const CLEANUP_POLL: u32 = 16_384;

#[cfg(target_arch = "mips")]
enum Wait {
    Matched,
    CdError,
    Timeout,
}

/// Blocking, polled CD-ROM sector reader.
///
/// Owns the state hl-psx kept in module-level `static mut`s: the
/// "first prepare has drained boot-time IRQs" flag and a one-sector bounce
/// buffer that unexpected `DataReady` IRQs are DMA-drained into (the data
/// FIFO must be emptied before the ack or the controller wedges). Create one
/// reader and reuse it for the program's lifetime; a fresh reader merely
/// repeats the harmless first-time drain.
///
/// Typical use is not these raw methods but [`load_chunk`] /
/// [`load_chunk_decompressed`] on top. The raw sequence is:
/// `prepare()` then `start_read(lba)` then N x `read_sector(&mut buf)` then
/// `stop()`.
/// `diag()` cause byte: the drive raised INT5; the snapshot carries the
/// error response's status and error-code bytes.
#[cfg(target_arch = "mips")]
pub const DIAG_CD_ERROR: u8 = 0x05;
/// `diag()` cause byte: the wait spun out; the snapshot carries the raw
/// `CD_STATUS` register and the last IRQ flag seen.
#[cfg(target_arch = "mips")]
pub const DIAG_TIMEOUT: u8 = 0xFF;
/// `diag()` cause byte: the parameter FIFO never freed up.
#[cfg(target_arch = "mips")]
pub const DIAG_PARAM_STUCK: u8 = 0xFE;
/// `diag()` command byte standing in for a `read_sector` wait (ReadN is
/// streaming; no command byte is in flight).
#[cfg(target_arch = "mips")]
pub const DIAG_SITE_READ: u8 = 0xD0;

#[cfg(target_arch = "mips")]
pub struct SectorReader {
    prepared: bool,
    /// Mode byte the last prepare() set; re-sent inside every BIOS-bracket
    /// read start, because that is what the BIOS does.
    mode: u8,
    discard: [u32; SECTOR_WORDS],
    /// Last failure snapshot: `[cause, status, command, flag-or-error]`.
    /// Written on every failure path so a caller with only a screen to
    /// print on (the demo-disc loader) can say what the drive did.
    diag: [u8; 4],
}

#[cfg(target_arch = "mips")]
impl SectorReader {
    /// A reader that has not yet drained boot-time IRQs. `const` so it can
    /// sit in a `static`.
    pub const fn new() -> Self {
        SectorReader {
            prepared: false,
            mode: CD_MODE_DOUBLE_SPEED_2048,
            discard: [0; SECTOR_WORDS],
            diag: [0; 4],
        }
    }

    /// The last failure snapshot packed big-endian:
    /// `cause<<24 | status<<16 | command<<8 | flag_or_error`. Zero when
    /// nothing has failed yet. Causes are the `DIAG_*` constants; for
    /// [`DIAG_CD_ERROR`] the status/flag bytes are the INT5 response pair,
    /// otherwise status is the raw `CD_STATUS` register at failure and the
    /// low byte is the last IRQ flag seen.
    pub fn diag(&self) -> u32 {
        u32::from_be_bytes(self.diag)
    }

    // --- register helpers (exact hl-psx port) ---

    #[inline]
    unsafe fn wr_index(&mut self, i: u8) {
        unsafe { psx_io::write8(CD_STATUS, i & 0x03) };
    }

    unsafe fn irq_flag(&mut self) -> u8 {
        unsafe {
            self.wr_index(1);
            let f = psx_io::read8(CD_IRQ) & 0x1F;
            self.wr_index(0);
            f
        }
    }

    unsafe fn ack(&mut self, irq: u8) {
        unsafe {
            self.wr_index(1);
            psx_io::write8(CD_IRQ, irq & 0x1F);
            psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
            self.wr_index(0);
        }
    }

    unsafe fn ack_all(&mut self) {
        unsafe {
            self.wr_index(1);
            psx_io::write8(CD_IRQ, 0x5F);
            psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
            self.wr_index(0);
        }
    }

    unsafe fn enable_irqs(&mut self) {
        unsafe {
            self.wr_index(1);
            psx_io::write8(CD_PARAM, 0x1F);
            self.wr_index(0);
        }
    }

    unsafe fn irq_enable(&mut self) -> u8 {
        unsafe {
            self.wr_index(0);
            let e = psx_io::read8(CD_IRQ) & 0x1F;
            self.wr_index(0);
            e
        }
    }

    unsafe fn set_irq_enable(&mut self, mask: u8) {
        unsafe {
            self.wr_index(1);
            psx_io::write8(CD_PARAM, mask & 0x1F);
            self.wr_index(0);
        }
    }

    unsafe fn drain_responses(&mut self) {
        // The response FIFO is 16 bytes deep, so a real drain reads at most 16.
        // Bound the loop: on heavy streaming the CD/emulator can wedge the FIFO
        // "not-empty", and an unbounded drain spins forever (hung loader).
        unsafe {
            self.wr_index(0);
            let mut guard = 0;
            while psx_io::read8(CD_STATUS) & STATUS_RESPONSE_FIFO_NOT_EMPTY != 0 && guard < 256 {
                let _ = psx_io::read8(CD_RESPONSE);
                guard += 1;
            }
        }
    }

    unsafe fn data_fifo_ready(&mut self) -> bool {
        unsafe {
            self.wr_index(0);
            psx_io::read8(CD_STATUS) & STATUS_DATA_FIFO_NOT_EMPTY != 0
        }
    }

    unsafe fn wait_param_room(&mut self) -> bool {
        let mut i = 0;
        while i < PARAM_POLL {
            if unsafe { psx_io::read8(CD_STATUS) } & STATUS_PARAMETER_FIFO_NOT_FULL != 0 {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Move one sector from the drive's buffer into RAM.
    ///
    /// This used to be a chopping-burst DMA on channel 3 (the hl-psx
    /// recipe). The CL1/CL2 silicon probes convicted that path on real
    /// hardware: the transfer is a state-dependent lottery, and the
    /// channel can latch its start bit and stay busy forever while
    /// moving nothing, which read as all-zero sectors everywhere the
    /// reader is used. PIO is the recipe the same probes proved
    /// byte-perfect on silicon: arm BFRD, wait until the data FIFO
    /// actually reports data, then pop all 2048 bytes. ~1.3 ms slower
    /// per sector than a working DMA, which no SectorReader user
    /// notices, and it cannot wedge the DMA controller.
    unsafe fn dma_read_sector(&mut self, buffer: *mut u32) {
        unsafe {
            // Arm the data transfer (BFRD).
            self.wr_index(0);
            psx_io::write8(CD_IRQ, 0x80);
            self.wr_index(0);
        }
        // The FIFO fills shortly after BFRD; the bound covers a slow
        // drive without letting a dead one hang the caller.
        let mut i = 0;
        while !unsafe { self.data_fifo_ready() } && i < DATA_POLL {
            i += 1;
        }
        for word_index in 0..SECTOR_WORDS {
            let b0 = unsafe { psx_io::read8(CD_DATA) } as u32;
            let b1 = unsafe { psx_io::read8(CD_DATA) } as u32;
            let b2 = unsafe { psx_io::read8(CD_DATA) } as u32;
            let b3 = unsafe { psx_io::read8(CD_DATA) } as u32;
            unsafe {
                buffer
                    .add(word_index)
                    .write_volatile((b3 << 24) | (b2 << 16) | (b1 << 8) | b0)
            };
        }
    }

    /// Clear an IRQ we were not waiting for. A stale `DataReady` must have its
    /// sector DMA-drained (into the reader's bounce buffer) before the ack,
    /// or the data FIFO stays occupied and later reads misalign.
    unsafe fn ack_unexpected(&mut self, flag: u8) {
        unsafe {
            match flag {
                IRQ_DATA_READY => {
                    let discard = self.discard.as_mut_ptr();
                    self.dma_read_sector(discard);
                    self.drain_responses();
                    self.ack(IRQ_DATA_READY);
                }
                IRQ_COMPLETE | IRQ_ACK | IRQ_DATA_END => {
                    self.drain_responses();
                    self.ack(flag);
                }
                _ => {
                    self.drain_responses();
                    self.ack_all();
                }
            }
        }
    }

    unsafe fn wait_irq(&mut self, expected: u8, limit: u32) -> Wait {
        let mut i = 0;
        while i < limit {
            let flag = unsafe { self.irq_flag() };
            if flag == expected {
                return Wait::Matched;
            }
            // Some drives raise the data FIFO before (or without) latching the
            // DataReady flag; treat visible data as a match.
            if expected == IRQ_DATA_READY && unsafe { self.data_fifo_ready() } {
                return Wait::Matched;
            }
            if flag == IRQ_ERROR {
                return Wait::CdError;
            }
            if flag != 0 {
                unsafe { self.ack_unexpected(flag) };
            }
            i += 1;
        }
        Wait::Timeout
    }

    /// Dispatch one command with controller IRQs masked and every FIFO in a
    /// known state, then wait for `expected` and ack it. The mask/ack ordering
    /// is load-bearing on silicon; do not reorder.
    unsafe fn send_command(
        &mut self,
        command: u8,
        params: &[u8],
        expected: u8,
        limit: u32,
    ) -> bool {
        unsafe {
            let saved = self.irq_enable();
            self.set_irq_enable(0);
            self.ack_all();
            self.wr_index(0);
            self.drain_responses();
            // Reset the parameter FIFO (0x40) before queueing parameters.
            self.wr_index(1);
            psx_io::write8(CD_IRQ, 0x40);
            self.wr_index(0);
            for &p in params {
                if !self.wait_param_room() {
                    self.wr_index(0);
                    self.diag = [DIAG_PARAM_STUCK, psx_io::read8(CD_STATUS), command, 0];
                    self.set_irq_enable(saved);
                    self.wr_index(0);
                    return false;
                }
                psx_io::write8(CD_PARAM, p);
            }
            psx_io::write8(CD_RESPONSE, command);
            let ok = match self.wait_irq(expected, limit) {
                Wait::Matched => {
                    self.drain_responses();
                    self.ack(expected);
                    true
                }
                Wait::CdError => {
                    // Capture the INT5 response pair (status, error code)
                    // before the drain throws it away.
                    self.wr_index(0);
                    let r0 = psx_io::read8(CD_RESPONSE);
                    let r1 = psx_io::read8(CD_RESPONSE);
                    self.diag = [DIAG_CD_ERROR, r0, command, r1];
                    self.drain_responses();
                    self.ack_all();
                    false
                }
                Wait::Timeout => {
                    self.wr_index(0);
                    let status = psx_io::read8(CD_STATUS);
                    let flag = self.irq_flag();
                    self.diag = [DIAG_TIMEOUT, status, command, flag];
                    false
                }
            };
            self.set_irq_enable(saved);
            self.wr_index(0);
            ok
        }
    }

    // --- public state machine, mirroring hl-psx hw::{prepare,start_read,read_sector,stop} ---

    /// Take over the CD controller for polled data reads and set
    /// double-speed / 2048-byte-sector mode.
    ///
    /// **Loud warning:** this rewrites `I_MASK` to VBlank-only and leaves it
    /// that way; see the module docs for the full list of side effects. Call
    /// it (directly or via [`load_chunk`]) only from code that owns interrupt
    /// policy, i.e. a polling main loop, not from an IRQ handler.
    ///
    /// Returns `false` when the Setmode handshake times out (no drive, tray
    /// open, dead controller); the reader is safe to retry.
    ///
    /// # Safety
    /// MMIO access; single-threaded use only (one live `SectorReader`, no
    /// concurrent CD/DMA-ch3 users, no CD-ROM IRQ handler installed). The
    /// caller accepts the global `I_MASK` rewrite.
    pub unsafe fn prepare(&mut self) -> bool {
        unsafe { self.prepare_with_mode(CD_MODE_DOUBLE_SPEED_2048) }
    }

    /// [`prepare`](Self::prepare) at single speed: half the throughput,
    /// twice the per-sector margin. The demo-disc chain loader measured
    /// silent payload corruption over hundreds of back-to-back
    /// double-speed sectors on the project console (2026-08-01, the
    /// loader's RAM checksum against the disc build); the header sector
    /// alone always read clean, so the failure scales with sustained
    /// rate, and a loader that takes three extra seconds beats one that
    /// jumps into a corrupt payload.
    ///
    /// # Safety
    /// Same contract as [`prepare`](Self::prepare).
    pub unsafe fn prepare_single_speed(&mut self) -> bool {
        unsafe { self.prepare_with_mode(0x00) }
    }

    unsafe fn prepare_with_mode(&mut self, mode: u8) -> bool {
        self.mode = mode;
        unsafe {
            // Keep CD-ROM at the controller level and poll its IRQ flags
            // manually, so DataReady cannot enter an unhandled CPU IRQ storm.
            psx_io::irq::set_mask(1 << psx_io::irq::source::VBLANK);
            psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
            self.enable_irqs();
            self.ack_all();
            psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);
            if !self.prepared {
                // A BIOS disc boot has already finished its file load. Do not
                // send Pause before our first stream; on real BIOS boot paths
                // some emulators have no active read command to pause and
                // never acknowledge it. Drain any already-latched data/ack
                // instead.
                let mut i = 0;
                while i < 16 {
                    let f = self.irq_flag();
                    if f == 0 {
                        break;
                    }
                    self.ack_unexpected(f);
                    i += 1;
                }
                self.ack_all();
                self.prepared = true;
            }
            // Purge whatever the previous tenant left in the data FIFO by
            // dropping BFRD (Request register, index 0: 0 = reset the data
            // FIFO). The demo-disc chain loader's header read came back
            // with shifted bytes on silicon (magic mismatch, identical
            // every attempt) after the menu's earlier disc reads; leftover
            // FIFO bytes are the only state that survives the IRQ drain
            // above, and an emulator FIFO never holds any, which is why
            // this cannot reproduce headless.
            self.wr_index(0);
            psx_io::write8(CD_IRQ, 0x00);
            self.send_command(CMD_SETMODE, &[mode], IRQ_ACK, ACK_POLL)
        }
    }

    /// Seek to `lba` (Setloc with BCD MSF) and start a ReadN stream.
    /// [`prepare`](Self::prepare) must have succeeded first.
    ///
    /// `lba` is relative to the start of this program's own disc image; on a
    /// multi-program disc [`psx_io::disc_base`] shifts it to where that image
    /// actually landed.
    ///
    /// # Safety
    /// Same contract as [`prepare`](Self::prepare).
    pub unsafe fn start_read(&mut self, lba: u32) -> bool {
        unsafe {
            let (m, s, f) = lba_to_bcd_msf(psx_io::disc_base::shift_lba(lba));
            if !self.send_command(CMD_SETLOC, &[m, s, f], IRQ_ACK, ACK_POLL) {
                return false;
            }
            if !self.send_command(CMD_READN, &[], IRQ_ACK, ACK_POLL) {
                return false;
            }
            self.enable_irqs();
            true
        }
    }

    /// [`start_read`](Self::start_read) the way the real BIOS starts one:
    /// SetLoc, then an EXPLICIT SeekL waited to completion, then ReadN.
    ///
    /// Traced from a real SCPH-1001 boot (2026-08-01, emulator CD command
    /// log): the BIOS brackets every read -- even a single sector -- as
    /// SetLoc/SeekL/SetMode/ReadN/Pause. The implicit seek a bare
    /// SetLoc+ReadN performs starts data flowing while the mech is still
    /// settling; the same console that corrupts our sustained implicit-seek
    /// streams loads 1.4 MB EXEs through the BIOS bracket without fault.
    ///
    /// # Safety
    /// Same contract as [`prepare`](Self::prepare).
    pub unsafe fn start_read_seek_first(&mut self, lba: u32, seek_poll: u32) -> bool {
        unsafe {
            let (m, s, f) = lba_to_bcd_msf(psx_io::disc_base::shift_lba(lba));
            if !self.send_command(CMD_SETLOC, &[m, s, f], IRQ_ACK, ACK_POLL) {
                return false;
            }
            // SeekL acks (INT3) then completes (INT2) once the head has
            // settled on the target; only then is ReadN issued, so the
            // drive never streams during mech settle.
            if !self.send_command(CMD_SEEKL, &[], IRQ_ACK, ACK_POLL) {
                return false;
            }
            match self.wait_irq(IRQ_COMPLETE, seek_poll) {
                Wait::Matched => {}
                Wait::CdError | Wait::Timeout => return false,
            }
            self.ack(IRQ_COMPLETE);
            // The BIOS re-sends SetMode inside every bracket, between the
            // seek completion and ReadN; every one of its ReadN commands in
            // the trace is prefixed SetLoc,SeekL,SetMode. Match it exactly.
            if !self.send_command(CMD_SETMODE, &[self.mode], IRQ_ACK, ACK_POLL) {
                return false;
            }
            if !self.send_command(CMD_READN, &[], IRQ_ACK, ACK_POLL) {
                return false;
            }
            self.enable_irqs();
            true
        }
    }

    /// Block until the next sector of the running ReadN stream is ready, then
    /// DMA its 2048 bytes into `buffer`. `false` on drive error or timeout
    /// (the stream is acked/cleaned up; follow with [`stop`](Self::stop)).
    ///
    /// # Safety
    /// Same contract as [`prepare`](Self::prepare); a read must be running
    /// (successful [`start_read`](Self::start_read)).
    pub unsafe fn read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> bool {
        unsafe {
            match self.wait_irq(IRQ_DATA_READY, DATA_POLL) {
                Wait::Matched => {}
                Wait::CdError => {
                    self.wr_index(0);
                    let r0 = psx_io::read8(CD_RESPONSE);
                    let r1 = psx_io::read8(CD_RESPONSE);
                    self.diag = [DIAG_CD_ERROR, r0, DIAG_SITE_READ, r1];
                    self.drain_responses();
                    self.ack_all();
                    return false;
                }
                Wait::Timeout => {
                    self.wr_index(0);
                    let status = psx_io::read8(CD_STATUS);
                    let flag = self.irq_flag();
                    self.diag = [DIAG_TIMEOUT, status, DIAG_SITE_READ, flag];
                    self.drain_responses();
                    self.ack_all();
                    return false;
                }
            }
            self.dma_read_sector(buffer.as_mut_ptr());
            self.drain_responses();
            self.ack(IRQ_DATA_READY);
            true
        }
    }

    /// Pause the ReadN stream (keeps the drive spun up) and ack everything.
    /// Safe to call after failures; it tolerates a drive with no active read.
    ///
    /// # Safety
    /// Same contract as [`prepare`](Self::prepare).
    pub unsafe fn stop(&mut self) {
        unsafe {
            if self.send_command(CMD_PAUSE, &[], IRQ_ACK, CLEANUP_POLL) {
                let _ = self.wait_irq(IRQ_COMPLETE, CLEANUP_POLL);
                self.drain_responses();
                self.ack(IRQ_COMPLETE);
            }
            self.ack_all();
        }
    }
}

#[cfg(target_arch = "mips")]
impl Default for SectorReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Binary `0..=99` to BCD, as CD-ROM commands expect.
#[cfg(any(target_arch = "mips", test))]
const fn bin_to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Data-track LBA to the absolute BCD `(minute, second, frame)` triple Setloc
/// takes. LBA 0 is 00:02:00 (the 150-sector / 2-second lead-in offset).
#[cfg(any(target_arch = "mips", test))]
fn lba_to_bcd_msf(lba: u32) -> (u8, u8, u8) {
    let abs = lba.saturating_add(150);
    (
        bin_to_bcd((abs / (60 * 75)) as u8),
        bin_to_bcd(((abs / 75) % 60) as u8),
        bin_to_bcd((abs % 75) as u8),
    )
}

/// The sector currently in `scratch`, as bytes (little-endian DMA words are
/// exactly the on-disc byte order).
#[cfg(target_arch = "mips")]
fn scratch_bytes(scratch: &[u32; SECTOR_WORDS]) -> &[u8] {
    // SAFETY: [u32; N] reinterpreted as its own bytes; alignment shrinks.
    unsafe { core::slice::from_raw_parts(scratch.as_ptr() as *const u8, SECTOR_BYTES) }
}

/// Read pack header sector `sector` (relative to `pack_lba`) into `scratch`,
/// unless `*loaded` says it is already there. Each miss is a full
/// prepare/seek/read/stop cycle, which is why sequential entry scans track
/// `loaded` (ports hl-psx's `load_pack_header_sector`).
#[cfg(target_arch = "mips")]
fn load_header_sector(
    rd: &mut SectorReader,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    sector: u32,
) -> bool {
    if *loaded == sector {
        return true;
    }
    // SAFETY: single-threaded polled MMIO; see SectorReader::prepare's contract,
    // which load_chunk/find_entry re-state to their callers.
    unsafe {
        if !rd.prepare() || !rd.start_read(pack_lba + sector) {
            rd.stop();
            return false;
        }
        let ok = rd.read_sector(scratch);
        rd.stop();
        if ok {
            *loaded = sector;
        }
        ok
    }
}

/// Read table entry `index` while scanning, stitching an entry that straddles
/// two header sectors (ports hl-psx's `read_pack_entry`; the sector math and
/// byte parsing are the crate's host-tested [`entry_location`] /
/// [`parse_entry_at`]).
#[cfg(target_arch = "mips")]
fn read_entry(
    rd: &mut SectorReader,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    header_sectors: u32,
    index: u32,
) -> Option<PackEntry> {
    let (sector, within) = entry_location(index);
    if sector >= header_sectors {
        return None;
    }
    if !load_header_sector(rd, pack_lba, scratch, loaded, sector) {
        return None;
    }
    if within + ENTRY_BYTES <= SECTOR_BYTES {
        parse_entry_at(scratch_bytes(scratch), within)
    } else {
        // Entry spans this sector and the next; stitch the 24 bytes together.
        if sector + 1 >= header_sectors {
            return None;
        }
        let first = SECTOR_BYTES - within;
        let mut stitched = [0u8; ENTRY_BYTES];
        stitched[..first].copy_from_slice(&scratch_bytes(scratch)[within..]);
        if !load_header_sector(rd, pack_lba, scratch, loaded, sector + 1) {
            return None;
        }
        stitched[first..].copy_from_slice(&scratch_bytes(scratch)[..ENTRY_BYTES - first]);
        parse_entry_at(&stitched, 0)
    }
}

/// Scan the pack table at `pack_lba` for `chunk_id` and return its entry
/// (including `byte_size` and the FNV `checksum` the writer stored).
///
/// Reads the header/table sectors through `scratch`, one at a time. `None`
/// on read failure, bad magic/version, or id not present. Inherits
/// [`SectorReader::prepare`]'s side effects (VBlank-only `I_MASK`).
#[cfg(target_arch = "mips")]
pub fn find_entry(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<PackEntry> {
    let mut loaded = u32::MAX;
    if !load_header_sector(rd, pack_lba, scratch, &mut loaded, 0) {
        return None;
    }
    let header = parse_header(scratch_bytes(scratch))?;
    let mut index = 0u32;
    while index < header.chunk_count {
        let entry = read_entry(
            rd,
            pack_lba,
            scratch,
            &mut loaded,
            header.header_sectors,
            index,
        )?;
        if entry.chunk_id == chunk_id {
            return Some(entry);
        }
        index += 1;
    }
    None
}

/// Stream chunk `chunk_id` from the pack at `pack_lba` into `dst`.
///
/// Scans the table for the entry (sector-by-sector through `scratch`,
/// straddle-safe), verifies the payload fits `dst`, then seeks to the payload
/// and copies it in whole sectors via `scratch`. Returns the chunk's exact
/// byte size, or `None` (no disc / not found / too big / read error). The
/// payload lands at `dst[0]` still compressed if the writer compressed it;
/// see [`load_chunk_decompressed`].
///
/// Reads only as many sectors as `byte_size` needs: the table's padded
/// `sector_count` could be garbage and looping on it would hang the loader.
///
/// Inherits [`SectorReader::prepare`]'s side effects: after this call
/// `I_MASK` is VBlank-only. No caching; scan cost is linear in the table, so
/// games loading many chunks should keep their own id table (see module doc).
#[cfg(target_arch = "mips")]
pub fn load_chunk(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let entry = find_entry(rd, pack_lba, chunk_id, scratch)?;
    let byte_size = entry.byte_size as usize;
    if byte_size > dst.len() * 4 {
        return None;
    }
    // SAFETY: same single-threaded polled-MMIO contract as find_entry above;
    // the byte copies stay inside dst (byte_size checked) and scratch.
    unsafe {
        if !rd.prepare() || !rd.start_read(pack_lba + entry.sector_offset) {
            rd.stop();
            return None;
        }
        let dst_ptr = dst.as_mut_ptr() as *mut u8;
        let needed = byte_size.div_ceil(SECTOR_BYTES);
        let mut s = 0usize;
        while s < needed {
            if !rd.read_sector(scratch) {
                rd.stop();
                return None;
            }
            let off = s * SECTOR_BYTES;
            let copy = byte_size.saturating_sub(off).min(SECTOR_BYTES);
            if copy > 0 {
                core::ptr::copy_nonoverlapping(
                    scratch.as_ptr() as *const u8,
                    dst_ptr.add(off),
                    copy,
                );
            }
            s += 1;
        }
        rd.stop();
    }
    Some(byte_size)
}

/// [`load_chunk`], then [`crate::decompress_hlzc_in_place`] on the result.
///
/// Returns the chunk's RAW byte length: the decompressed size for an `HLZC`
/// chunk, the stored size for an uncompressed one (raw passthrough). `None`
/// on any load or decode failure. `dst` must hold the raw payload plus the
/// in-place LZ4 margin (see the decompressor's docs); a too-small buffer
/// fails cleanly.
#[cfg(target_arch = "mips")]
pub fn load_chunk_decompressed(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let loaded = load_chunk(rd, pack_lba, chunk_id, scratch, dst)?;
    // SAFETY: [u32] viewed as its own bytes for the in-place decoder.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, dst.len() * 4) };
    crate::decompress_hlzc_in_place(bytes, loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_matches_the_disc_math() {
        // LBA 0 = absolute 00:02:00 (150-sector lead-in).
        assert_eq!(lba_to_bcd_msf(0), (0x00, 0x02, 0x00));
        // The default pack LBA: 1024 + 150 = 1174 = 15 * 75 + 49.
        assert_eq!(lba_to_bcd_msf(WORLD_PACK_DEFAULT_LBA), (0x00, 0x15, 0x49));
        // One full minute of sectors: 4500 - 150 = LBA 4350 -> 01:00:00.
        assert_eq!(lba_to_bcd_msf(4350), (0x01, 0x00, 0x00));
        // BCD digits, not binary: 59 seconds encodes as 0x59.
        assert_eq!(lba_to_bcd_msf(4350 - 75), (0x00, 0x59, 0x00));
    }

    #[test]
    fn default_pack_lba_matches_the_writer() {
        // psx-iso (the pack writer) is a host-only crate, so the guest-side
        // constant is mirrored here; this pins them together.
        assert_eq!(
            WORLD_PACK_DEFAULT_LBA,
            psx_iso::WORLD_PACK_DEFAULT_START_LBA
        );
    }
}
