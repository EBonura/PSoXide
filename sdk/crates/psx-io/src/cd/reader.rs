// SPDX-License-Identifier: GPL-2.0-or-later
//! Polled CD-ROM data-sector reads: seek, stream and pop 2048-byte sectors.
//!
//! [`SectorReader`] holds the [`Cd`] token and drives the controller through
//! its register steps, so the register map, the IRQ acknowledge order and the
//! BCD arithmetic exist once, in [`super`]. What it adds is the command
//! sequence the pack loader and the FMV streamer have run on silicon: a
//! SetMode-then-Setloc-then-ReadN start with every IRQ masked and every FIFO
//! in a known state, a wait loop that drains stale responses (and a stale
//! data sector into a bounce buffer) before it acknowledges them, and a Pause
//! that leaves the drive spun up. The comments that record silicon findings
//! travel with the code.
//!
//! Sectors come out of the data FIFO by CPU reads (PIO), not DMA channel 3.
//! The CL1/CL2 silicon probes convicted the chopping-burst DMA path on real
//! hardware: the transfer is a state-dependent lottery, and the channel can
//! latch its start bit and stay busy forever while moving nothing, which read
//! as all-zero sectors. PIO is the recipe the same probes proved byte-perfect:
//! arm the data request, wait until the FIFO reports data, pop all 2048 bytes.
//! It costs about 1.3 ms more per sector than a working DMA, and it cannot
//! wedge the DMA controller.
//!
//! # What `prepare` does to the machine
//!
//! [`SectorReader::prepare`] polls the controller's own IRQ flags, so it
//! keeps CD-ROM interrupts away from the CPU for the length of a stream:
//!
//! * `I_MASK` is set to VBlank-only (by `prepare`, and again by each
//!   `start_read`) until [`SectorReader::stop`] puts the previous value back. Every other source stops reaching the CPU in
//!   between: a CD-ROM IRQ with no handler would otherwise be an unhandled-IRQ
//!   storm.
//! * The latched CD-ROM `I_STAT` bit is acked and the controller's five IRQ
//!   enables are switched on, then every pending controller IRQ is acked.
//! * On the first `prepare` of a reader, IRQs latched by earlier activity (the
//!   BIOS disc boot's file load) are drained. Pausing instead would hang on
//!   an emulator that has no active read to pause.
//! * `SetMode` is sent: double speed and 2048-byte user data by default.
//!
//! No DMA channel is enabled: nothing here uses one.

use super::lba_to_bcd_msf;
use crate::irq;
use crate::periph::Cd;
use psx_hw::cd::irq::{
    ACKNOWLEDGE as IRQ_ACK, COMPLETE as IRQ_COMPLETE, DATA_END as IRQ_DATA_END,
    DATA_READY as IRQ_DATA_READY, ERROR as IRQ_ERROR,
};
use psx_hw::cd::{
    CMD_DEMUTE, CMD_PAUSE, CMD_READN, CMD_SEEKL, CMD_SETFILTER, CMD_SETLOC, CMD_SETMODE,
    MODE_DOUBLE_SPEED,
};

/// One CD sector's user data in 32-bit words.
pub const SECTOR_WORDS: usize = 2048 / 4;

/// `SetMode` byte for double-speed 2048-byte sectors.
const MODE_DATA_DOUBLE_SPEED: u8 = MODE_DOUBLE_SPEED;

// Poll limits, tuned on silicon. `DATA_POLL` covers a worst-case seek at
// double speed.
const ACK_POLL: u32 = 16_384;
const PARAM_POLL: u32 = 16_384;
const DATA_POLL: u32 = 4_000_000;
const CLEANUP_POLL: u32 = 16_384;
/// Response bytes drained at most. The FIFO holds 16, but on heavy streaming
/// the controller (or an emulator) can wedge it "not empty", and an unbounded
/// drain would hang the loader.
const DRAIN_LIMIT: u32 = 256;

/// Every controller IRQ enable, which `prepare` and a started read switch on.
const ALL_IRQS_ENABLED: u8 = psx_hw::cd::irq::ALL;

/// [`SectorReader::diagnostics`] cause byte: the drive raised INT5; the
/// snapshot carries the error response's status and error-code bytes.
pub const DIAG_CD_ERROR: u8 = 0x05;
/// [`SectorReader::diagnostics`] cause byte: the wait spun out; the snapshot
/// carries the raw status register and the last IRQ flag seen.
pub const DIAG_TIMEOUT: u8 = 0xFF;
/// [`SectorReader::diagnostics`] cause byte: the parameter FIFO never freed up.
pub const DIAG_PARAM_STUCK: u8 = 0xFE;
/// [`SectorReader::diagnostics`] command byte standing in for a `read_sector`
/// wait (ReadN is streaming; no command byte is in flight).
pub const DIAG_SITE_READ: u8 = 0xD0;

enum Wait {
    Matched,
    CdError,
    Timeout,
}

/// The interrupt mask a stream replaced, kept until it ends.
#[derive(Debug, Default)]
struct SavedMask(Option<u32>);

impl SavedMask {
    /// Remember `current`, unless a mask is already kept: a second `prepare`
    /// before `stop` sees the stream's own VBlank-only mask, which is not the
    /// caller's.
    fn keep(&mut self, current: u32) {
        if self.0.is_none() {
            self.0 = Some(current);
        }
    }

    fn take(&mut self) -> Option<u32> {
        self.0.take()
    }
}

/// Blocking, polled CD-ROM sector reader.
///
/// Owns the CD token, plus the state a polled stream needs: whether the
/// first `prepare` has drained boot-time IRQs, the interrupt mask to put back
/// at `stop`, and a one-sector bounce buffer that an unexpected data-ready IRQ
/// is drained into (the data FIFO must be emptied before the ack or the
/// controller wedges). Create one reader and reuse it for the program's
/// lifetime; a fresh reader merely repeats the harmless first-time drain.
///
/// Typical use is not these raw methods but `psx_pack::cd::load_chunk` on top.
/// The raw sequence is: `prepare()`, `start_read(lba)`, N times
/// `read_sector(&mut buf)`, `stop()`.
///
/// The reader owns the drive, so nothing else can command it meanwhile:
///
/// ```compile_fail,E0382
/// use psx_io::cd::reader::SectorReader;
/// use psx_io::periph::Cd;
///
/// // SAFETY: the doctest never runs a command; it only has to compile.
/// let mut cd = unsafe { Cd::steal() };
/// let mut reader = SectorReader::with_cd(cd);
/// // The token moved into the reader, so this is a use after move.
/// let _ = cd.play_track(2);
/// reader.stop();
/// ```
pub struct SectorReader {
    cd: Cd,
    prepared: bool,
    /// Mode byte the last prepare() set; re-sent inside every BIOS-bracket
    /// read start, because that is what the BIOS does.
    mode: u8,
    saved_mask: SavedMask,
    discard: [u32; SECTOR_WORDS],
    /// Last failure snapshot: `[cause, status, command, flag-or-error]`.
    /// Written on every failure path so a caller with only a screen to
    /// print on (the demo-disc loader) can say what the drive did.
    diag: [u8; 4],
}

impl SectorReader {
    /// A reader that has not yet drained boot-time IRQs. `const` so it can
    /// sit in a `static`.
    pub const fn with_cd(cd: Cd) -> Self {
        SectorReader {
            cd,
            prepared: false,
            mode: MODE_DATA_DOUBLE_SPEED,
            saved_mask: SavedMask(None),
            discard: [0; SECTOR_WORDS],
            diag: [0; 4],
        }
    }

    /// [`with_cd`](Self::with_cd) on a token the caller does not hold.
    #[deprecated(note = "use `SectorReader::with_cd` with the `Cd` token")]
    pub const fn new() -> Self {
        // SAFETY: a token is a logic guard, not a memory-safety one (see
        // `crate::periph`), and the old constructor never took one.
        Self::with_cd(unsafe { Cd::steal() })
    }

    /// Give the token back, restoring the interrupt mask first if a stream
    /// is still open. Call [`stop`](Self::stop) first to also pause the drive.
    pub fn release(mut self) -> Cd {
        self.restore_irq_mask();
        self.cd
    }

    /// The token, lent for commands outside the reader's own sequences (the
    /// drive's audio mixer, a status poll). The reader stays borrowed meanwhile.
    pub fn cd_mut(&mut self) -> &mut Cd {
        &mut self.cd
    }

    /// The last failure snapshot packed big-endian:
    /// `cause<<24 | status<<16 | command<<8 | flag_or_error`. Zero when
    /// nothing has failed yet. Causes are the `DIAG_*` constants; for
    /// [`DIAG_CD_ERROR`] the status/flag bytes are the INT5 response pair,
    /// otherwise status is the raw status register at failure and the low
    /// byte is the last IRQ flag seen.
    pub fn diagnostics(&self) -> u32 {
        u32::from_be_bytes(self.diag)
    }

    /// Renamed to [`SectorReader::diagnostics`].
    #[deprecated(note = "renamed to `diagnostics`")]
    #[inline(always)]
    pub fn diag(&self) -> u32 {
        self.diagnostics()
    }

    /// Pop one sector's 2048 bytes from the data FIFO into `buffer`.
    ///
    /// Arms the data request, waits (bounded) until the FIFO reports data,
    /// then pops byte by byte. The words are written with volatile stores, in
    /// order, so the pops and the stores interleave as on silicon.
    fn pop_sector(cd: &mut Cd, buffer: &mut [u32; SECTOR_WORDS]) {
        cd.request_data();
        // The FIFO fills shortly after the request; the bound covers a slow
        // drive without letting a dead one hang the caller.
        let mut spins = 0;
        while !cd.is_data_fifo_ready() && spins < DATA_POLL {
            spins += 1;
        }
        for word in buffer.iter_mut() {
            let b0 = cd.read_data_byte() as u32;
            let b1 = cd.read_data_byte() as u32;
            let b2 = cd.read_data_byte() as u32;
            let b3 = cd.read_data_byte() as u32;
            // SAFETY: `word` is a live, aligned `&mut u32` into `buffer`.
            unsafe { core::ptr::write_volatile(word, (b3 << 24) | (b2 << 16) | (b1 << 8) | b0) };
        }
    }

    /// Clear an IRQ we were not waiting for. A stale `DataReady` must have its
    /// sector popped (into the reader's bounce buffer) before the ack, or the
    /// data FIFO stays occupied and later reads misalign.
    fn ack_unexpected(&mut self, flag: u8) {
        match flag {
            IRQ_DATA_READY => {
                Self::pop_sector(&mut self.cd, &mut self.discard);
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_irq(IRQ_DATA_READY);
            }
            IRQ_COMPLETE | IRQ_ACK | IRQ_DATA_END => {
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_irq(flag);
            }
            _ => {
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_all_and_reset_parameters();
            }
        }
    }

    fn wait_irq(&mut self, expected: u8, limit: u32) -> Wait {
        let mut spins = 0;
        while spins < limit {
            let flag = self.cd.irq_flag_value();
            if flag == expected {
                return Wait::Matched;
            }
            // Some drives raise the data FIFO before (or without) latching the
            // DataReady flag; treat visible data as a match.
            if expected == IRQ_DATA_READY && self.cd.is_data_fifo_ready() {
                return Wait::Matched;
            }
            if flag == IRQ_ERROR {
                return Wait::CdError;
            }
            if flag != 0 {
                self.ack_unexpected(flag);
            }
            spins += 1;
        }
        Wait::Timeout
    }

    /// Record an INT5: its response pair (status, error code) is read before
    /// the drain throws it away.
    fn note_drive_error(&mut self, command: u8) {
        let status = self.cd.read_response_byte();
        let code = self.cd.read_response_byte();
        self.diag = [DIAG_CD_ERROR, status, command, code];
    }

    /// Record a wait that spun out.
    fn note_timeout(&mut self, command: u8) {
        let status = self.cd.status_register();
        let flag = self.cd.irq_flag_value();
        self.diag = [DIAG_TIMEOUT, status, command, flag];
    }

    /// Dispatch one command with controller IRQs masked and every FIFO in a
    /// known state, then wait for `expected` and ack it. The mask/ack ordering
    /// is load-bearing on silicon; do not reorder.
    fn send_command(&mut self, command: u8, params: &[u8], expected: u8, limit: u32) -> bool {
        let saved = self.cd.irq_enable_mask();
        self.cd.set_irq_enable_mask(0);
        self.cd.acknowledge_all_and_reset_parameters();
        self.cd.drain_response_limited(DRAIN_LIMIT);
        // Reset the parameter FIFO before queueing parameters.
        self.cd.reset_parameter_fifo();
        for &param in params {
            if !self.cd.wait_parameter_room(PARAM_POLL) {
                self.diag = [DIAG_PARAM_STUCK, self.cd.status_register(), command, 0];
                self.cd.set_irq_enable_mask(saved);
                return false;
            }
            self.cd.send_parameter_byte(param);
        }
        self.cd.send_command_byte(command);
        let ok = match self.wait_irq(expected, limit) {
            Wait::Matched => {
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_irq(expected);
                true
            }
            Wait::CdError => {
                self.note_drive_error(command);
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_all_and_reset_parameters();
                false
            }
            Wait::Timeout => {
                self.note_timeout(command);
                false
            }
        };
        self.cd.set_irq_enable_mask(saved);
        ok
    }

    /// Set `I_MASK` to VBlank-only, keeping the caller's mask for
    /// [`stop`](Self::stop). Idempotent within a stream.
    fn enter_polling_mask(&mut self) {
        self.saved_mask.keep(irq::mask());
        irq::set_mask(1 << psx_hw::irq::source::VBLANK);
    }

    fn restore_irq_mask(&mut self) {
        if let Some(mask) = self.saved_mask.take() {
            irq::set_mask(mask);
        }
    }

    /// Take over the controller for polled data reads and set
    /// double-speed / 2048-byte-sector mode.
    ///
    /// Sets `I_MASK` to VBlank-only until [`stop`](Self::stop) puts the
    /// previous mask back; see the module docs for the full list of side
    /// effects. Call it only from code that owns interrupt policy, a polling
    /// main loop, and not from an IRQ handler. No CD-ROM IRQ handler may be
    /// installed while a stream runs.
    ///
    /// Returns `false` when the Setmode handshake times out (no drive, tray
    /// open, dead controller); the reader is safe to retry.
    pub fn prepare(&mut self) -> bool {
        self.prepare_with_mode(MODE_DATA_DOUBLE_SPEED)
    }

    /// [`prepare`](Self::prepare) at single speed: half the throughput,
    /// twice the per-sector margin. The demo-disc chain loader measured
    /// silent payload corruption over hundreds of back-to-back
    /// double-speed sectors on the project console (2026-08-01, the
    /// loader's RAM checksum against the disc build); the header sector
    /// alone always read clean, so the failure scales with sustained
    /// rate, and a loader that takes three extra seconds beats one that
    /// jumps into a corrupt payload.
    pub fn prepare_single_speed(&mut self) -> bool {
        self.prepare_with_mode(0x00)
    }

    /// [`prepare`](Self::prepare) with an explicit Setmode byte, for
    /// streams that need more than plain data: e.g. `0x80 | 0x40 | 0x08`
    /// (double speed, XA-ADPCM on, file/channel filter) plays interleaved
    /// XA audio through the SPU while video sectors still arrive as data.
    pub fn prepare_mode(&mut self, mode: u8) -> bool {
        self.prepare_with_mode(mode)
    }

    /// Setfilter: the XA file and channel whose audio sectors the drive
    /// plays when the mode has the filter bit set.
    pub fn set_filter(&mut self, file: u8, channel: u8) -> bool {
        self.send_command(CMD_SETFILTER, &[file, channel], IRQ_ACK, ACK_POLL)
    }

    /// Unmute the drive: let CD-DA and XA-ADPCM reach the SPU. The drive
    /// stays muted across programs, so a stream that plays XA audio must not
    /// assume the last tenant left it unmuted.
    #[doc(alias = "Demute")]
    pub fn unmute(&mut self) -> bool {
        self.send_command(CMD_DEMUTE, &[], IRQ_ACK, ACK_POLL)
    }

    fn prepare_with_mode(&mut self, mode: u8) -> bool {
        self.mode = mode;
        // Keep CD-ROM at the controller level and poll its IRQ flags
        // manually, so DataReady cannot enter an unhandled CPU IRQ storm.
        self.enter_polling_mask();
        irq::acknowledge(1 << psx_hw::irq::source::CDROM);
        self.cd.set_irq_enable_mask(ALL_IRQS_ENABLED);
        self.cd.acknowledge_all_and_reset_parameters();
        if !self.prepared {
            // A BIOS disc boot has already finished its file load. Do not
            // send Pause before our first stream; on real BIOS boot paths
            // some emulators have no active read command to pause and
            // never acknowledge it. Drain any already-latched data/ack
            // instead.
            let mut drained = 0;
            while drained < 16 {
                let flag = self.cd.irq_flag_value();
                if flag == 0 {
                    break;
                }
                self.ack_unexpected(flag);
                drained += 1;
            }
            self.cd.acknowledge_all_and_reset_parameters();
            self.prepared = true;
        }
        // Purge whatever the previous tenant left in the data FIFO by
        // dropping the data request (writing 0 resets the data FIFO). The
        // demo-disc chain loader's header read came back with shifted bytes
        // on silicon (magic mismatch, identical every attempt) after the
        // menu's earlier disc reads; leftover FIFO bytes are the only state
        // that survives the IRQ drain above, and an emulator FIFO never holds
        // any, which is why this cannot reproduce headless.
        self.cd.clear_data_request();
        self.send_command(CMD_SETMODE, &[mode], IRQ_ACK, ACK_POLL)
    }

    /// Setloc to a program-relative `lba`, shifted by [`crate::disc_base`]
    /// to where this program's image actually landed on a multi-program disc.
    fn set_location(&mut self, lba: u32) -> bool {
        let [minute, second, frame] = lba_to_bcd_msf(crate::disc_base::shift_lba(lba));
        self.send_command(CMD_SETLOC, &[minute, second, frame], IRQ_ACK, ACK_POLL)
    }

    /// Seek to `lba` (Setloc with BCD MSF) and start a ReadN stream.
    /// [`prepare`](Self::prepare) must have succeeded first.
    ///
    /// `lba` is relative to the start of this program's own disc image; on a
    /// multi-program disc [`crate::disc_base`] shifts it to where that image
    /// actually landed.
    pub fn start_read(&mut self, lba: u32) -> bool {
        // A caller that prepared once and starts several streams still gets
        // the polling mask for each, and `stop` still puts its own back.
        self.enter_polling_mask();
        if !self.set_location(lba) {
            return false;
        }
        if !self.send_command(CMD_READN, &[], IRQ_ACK, ACK_POLL) {
            return false;
        }
        self.cd.set_irq_enable_mask(ALL_IRQS_ENABLED);
        true
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
    pub fn start_read_seek_first(&mut self, lba: u32, seek_poll: u32) -> bool {
        self.enter_polling_mask();
        if !self.set_location(lba) {
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
        self.cd.acknowledge_irq(IRQ_COMPLETE);
        // The BIOS re-sends SetMode inside every bracket, between the
        // seek completion and ReadN; every one of its ReadN commands in
        // the trace is prefixed SetLoc,SeekL,SetMode. Match it exactly.
        let mode = self.mode;
        if !self.send_command(CMD_SETMODE, &[mode], IRQ_ACK, ACK_POLL) {
            return false;
        }
        if !self.send_command(CMD_READN, &[], IRQ_ACK, ACK_POLL) {
            return false;
        }
        self.cd.set_irq_enable_mask(ALL_IRQS_ENABLED);
        true
    }

    /// Block until the next sector of the running ReadN stream is ready, then
    /// pop its 2048 bytes into `buffer`. `false` on drive error or timeout
    /// (the stream is acked/cleaned up; follow with [`stop`](Self::stop)). A
    /// read must be running (a successful [`start_read`](Self::start_read)).
    pub fn read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> bool {
        match self.wait_irq(IRQ_DATA_READY, DATA_POLL) {
            Wait::Matched => {}
            Wait::CdError => {
                self.note_drive_error(DIAG_SITE_READ);
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_all_and_reset_parameters();
                return false;
            }
            Wait::Timeout => {
                self.note_timeout(DIAG_SITE_READ);
                self.cd.drain_response_limited(DRAIN_LIMIT);
                self.cd.acknowledge_all_and_reset_parameters();
                return false;
            }
        }
        Self::pop_sector(&mut self.cd, buffer);
        self.cd.drain_response_limited(DRAIN_LIMIT);
        self.cd.acknowledge_irq(IRQ_DATA_READY);
        true
    }

    /// Non-blocking [`read_sector`](Self::read_sector): check the controller
    /// once and, if the next sector of the running ReadN stream is ready,
    /// pop its 2048 bytes into `buffer` and ack it.
    ///
    /// `Ok(true)` means `buffer` holds a new sector, `Ok(false)` that none
    /// has arrived yet. `Err(())` is a drive error; the stream is acked and
    /// the caller should [`stop`](Self::stop) (the diag snapshot is set as
    /// for `read_sector`).
    ///
    /// Streaming consumers (FMV) call this between units of other work so
    /// the drive never runs ahead of the CPU by more than the controller can
    /// hold. At double speed a sector lands every ~6.7 ms.
    #[allow(
        clippy::result_unit_err,
        reason = "the failure detail is in `diagnostics`; callers only branch on it"
    )]
    pub fn try_read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> Result<bool, ()> {
        let flag = self.cd.irq_flag_value();
        if flag == IRQ_ERROR {
            self.note_drive_error(DIAG_SITE_READ);
            self.cd.drain_response_limited(DRAIN_LIMIT);
            self.cd.acknowledge_all_and_reset_parameters();
            return Err(());
        }
        if flag != IRQ_DATA_READY && !self.cd.is_data_fifo_ready() {
            if flag != 0 {
                self.ack_unexpected(flag);
            }
            return Ok(false);
        }
        Self::pop_sector(&mut self.cd, buffer);
        self.cd.drain_response_limited(DRAIN_LIMIT);
        self.cd.acknowledge_irq(IRQ_DATA_READY);
        Ok(true)
    }

    /// Pause the ReadN stream (keeps the drive spun up), ack everything and
    /// put the interrupt mask [`prepare`](Self::prepare) replaced back.
    /// Safe to call after failures; it tolerates a drive with no active read.
    pub fn stop(&mut self) {
        if self.send_command(CMD_PAUSE, &[], IRQ_ACK, CLEANUP_POLL) {
            let _ = self.wait_irq(IRQ_COMPLETE, CLEANUP_POLL);
            self.cd.drain_response_limited(DRAIN_LIMIT);
            self.cd.acknowledge_irq(IRQ_COMPLETE);
        }
        self.cd.acknowledge_all_and_reset_parameters();
        // Last, so a CD-ROM IRQ still latched cannot reach the CPU through
        // the restored mask.
        self.restore_irq_mask();
    }
}

#[allow(
    deprecated,
    reason = "the old no-argument constructor is what Default always meant"
)]
impl Default for SectorReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_owns_the_token_and_hands_it_back() {
        // SAFETY: a test-local token on the host. Releasing a reader that never
        // prepared touches no register (no mask is saved to restore).
        let reader = SectorReader::with_cd(unsafe { Cd::steal() });
        assert_eq!(reader.diagnostics(), 0);
        let _token: Cd = reader.release();
    }

    #[test]
    fn a_reader_fits_a_static_because_the_constructor_is_const() {
        // SAFETY: a test-local token on the host; the static is never used.
        static _READER: SectorReader = SectorReader::with_cd(unsafe { Cd::steal() });
    }

    #[test]
    fn a_second_prepare_keeps_the_callers_mask_not_the_streams() {
        let mut saved = SavedMask::default();
        saved.keep(0x0000_0305); // the caller's mask
        saved.keep(1 << psx_hw::irq::source::VBLANK); // a second prepare's view
        assert_eq!(saved.take(), Some(0x0000_0305));
        assert_eq!(saved.take(), None, "stop restores once");
        saved.keep(0x42);
        assert_eq!(saved.take(), Some(0x42), "a new stream saves afresh");
    }

    #[test]
    fn the_sector_is_2048_bytes_of_words() {
        assert_eq!(SECTOR_WORDS * 4, 2048);
    }
}
