//! CD-ROM drive: commands, responses, sector polling, CD-DA playback in
//! [`audio`], XA-ADPCM music in [`xa`] and polled data-sector reads in
//! [`reader`].
//!
//! One driver owns the controller: the [`Cd`] token. Every command and
//! register step is a method taking `&mut Cd`, so the borrow checker sees a
//! second driver that tries to program the controller while one is mid-command.
//! [`xa::Player`] and [`reader::SectorReader`] hold the token while they exist
//! and give it back with `release`.
//!
//! The controller exposes four byte registers selected by the low two
//! bits of the index register at [`BASE`]. Register addresses, command bytes
//! and status bits live in [`psx_hw::cd`].

pub mod audio;
pub mod reader;
pub mod xa;

use crate::periph::Cd;
use crate::{irq, read_u8, write_u8};

use psx_hw::cd::{
    BASE, CMD_DEMUTE, CMD_GETLOCP, CMD_GETSTAT, CMD_MUTE, CMD_PAUSE, CMD_PLAY, CMD_READN,
    CMD_SETLOC, CMD_SETMODE, CMD_STOP, STAT_PLAYING, STAT_SEEKING,
};

const REG_INDEX: u32 = BASE;
const REG_COMMAND_RESPONSE: u32 = BASE + 1;
const REG_PARAMETER: u32 = BASE + 2;
const REG_REQUEST_IRQ: u32 = BASE + 3;

const STATUS_PARAM_NOT_FULL: u8 = 1 << 4;
const STATUS_RESPONSE_NOT_EMPTY: u8 = 1 << 5;
const IRQ_ACK: u8 = 3;
const IRQ_DATA_READY: u8 = 1;
const IRQ_COMPLETE: u8 = 2;
const IRQ_DATA_END: u8 = 4;
const IRQ_ERROR: u8 = 5;
const IRQ_ACK_ALL: u8 = 0x1F;
const IRQ_PARAM_FIFO_RESET: u8 = 0x40;
const STATUS_DATA_FIFO_NOT_EMPTY: u8 = 1 << 6;
/// Request register, index 0: arm the data FIFO (BFRD) so the next sector's
/// bytes can be popped; writing 0 drops it again.
const REQUEST_DATA: u8 = 0x80;
/// Response bytes a sector probe discards at most. The FIFO holds 16, but a
/// controller wedged "not empty" must not be able to spin the drain forever.
const SECTOR_POLL_DRAIN_LIMIT: u32 = 256;

/// Fixed-size command response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Response {
    bytes: [u8; 16],
    len: usize,
}

impl Response {
    /// A response with no bytes, what a deprecated forwarder hands back where
    /// the command it wraps now reports a [`CdError`].
    pub const fn empty() -> Self {
        Response {
            bytes: [0; 16],
            len: 0,
        }
    }

    /// Number of response bytes captured.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no response bytes were captured.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Response bytes in FIFO order.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Why a CD command produced no response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdError {
    /// The controller did not free a parameter slot or answer within the poll
    /// budget (no drive, no disc, or a wedged controller).
    Timeout,
    /// The drive answered with an error (INT5).
    DriveError,
}

/// Poll budget the blocking commands give the controller for each wait (a
/// parameter slot, then the acknowledge). Generous: the controller frees a
/// slot and acknowledges within microseconds to milliseconds, so this is a
/// hang guard, not a latency.
pub const DEFAULT_COMMAND_SPINS: u32 = 131_072;

impl Cd {
    /// Send a command and return its first response packet, waiting at most
    /// [`DEFAULT_COMMAND_SPINS`] polls for each step.
    ///
    /// Returns [`CdError::Timeout`] when the controller never answers and
    /// [`CdError::DriveError`] when the drive reports an error. A timed-out
    /// command leaves CD-ROM IRQ output masked, as [`Self::try_command`] does.
    pub fn command(&mut self, command: u8, params: &[u8]) -> Result<Response, CdError> {
        self.command_within(command, params, DEFAULT_COMMAND_SPINS)
    }

    /// [`Self::command`] with an explicit poll budget per step.
    pub fn command_within(
        &mut self,
        command: u8,
        params: &[u8],
        spin_limit: u32,
    ) -> Result<Response, CdError> {
        run_command(&mut Mmio, command, params, spin_limit)
    }

    /// Try to send a command and capture its first response packet.
    ///
    /// Returns `None` if the controller does not expose parameter room or
    /// a response within `spin_limit` polls, or if the drive reports an error
    /// ([`Self::command_within`] tells the two apart). Use this for gameplay paths
    /// where a not-ready drive should not stall rendering forever. If a
    /// dispatched command times out, CD-ROM IRQ output remains masked so a
    /// late ACK cannot interrupt a polling caller.
    pub fn try_command(&mut self, command: u8, params: &[u8], spin_limit: u32) -> Option<Response> {
        self.command_within(command, params, spin_limit).ok()
    }

    /// Current CD-ROM IRQ flag value (0 = none, 1 = data ready, 2 = complete,
    /// 3 = ack, 5 = error).
    ///
    /// Exposed so callers can build their own wait loops bounded by a hardware
    /// timer rather than by a poll count. A poll budget is only a proxy for time
    /// and drifts with CPU and bus speed, which matters when the thing being
    /// measured is mechanical.
    pub fn irq_flag_value(&mut self) -> u8 {
        irq_flag()
    }

    /// Acknowledge the given CD-ROM IRQ bits.
    pub fn acknowledge_irq(&mut self, bits: u8) {
        ack_irq(bits);
    }

    /// Drain any pending response bytes, discarding them.
    pub fn discard_response(&mut self) {
        drain_response_fifo();
    }

    /// Send a command without waiting for any response. Pairs with
    /// [`Self::irq_flag_value`] for caller-timed waits.
    ///
    /// Returns the CD-ROM IRQ enable to give back to
    /// [`restore_irq_output`](Self::restore_irq_output), or `None` if the
    /// parameter FIFO never made room. Leaves CD-ROM IRQ
    /// output masked exactly as the polled helpers do, so a late ACK cannot
    /// interrupt the caller mid-measurement; call [`Self::restore_irq_output`] when done.
    pub fn dispatch_command(&mut self, command: u8, params: &[u8], spin_limit: u32) -> Option<u8> {
        let irq_enable = begin_polled_command();
        select_index(0);
        for &param in params {
            if !wait_param_room_bounded(spin_limit) {
                finish_failed_polled_command(irq_enable);
                return None;
            }
            write_byte(REG_PARAMETER, param);
        }
        write_byte(REG_COMMAND_RESPONSE, command);
        Some(irq_enable)
    }

    /// Restore the CD-ROM IRQ enable saved by [`Self::dispatch_command`].
    pub fn restore_irq_output(&mut self, saved: u8) {
        drain_response_fifo();
        ack_irq(IRQ_ACK_ALL);
        restore_irq_enable(saved);
        select_index(0);
    }

    /// Nonblocking readiness probe for a sector reader that owns the INT1 ACK.
    /// Data-ready is never acknowledged here. Other completion responses are
    /// drained (at most 256 bytes) and acknowledged; drive errors also reset the
    /// parameter FIFO. A ready data FIFO is accepted if no classified IRQ remains.
    pub fn poll_data_sector(&mut self) -> Result<bool, SectorPollError> {
        poll_sector(&mut SectorPollMmio)
    }

    /// Wait for the next streamed data sector (INT1) and acknowledge it.
    ///
    /// Use between [`Self::try_start_reading`] and [`Self::try_pause_until_complete`] to step through
    /// a sector stream. Unrelated pending IRQs are drained and acknowledged so a
    /// stale response cannot be mistaken for a sector arrival. Returns `false` on
    /// a drive error or if no sector arrives within `spin_limit` polls.
    pub fn try_wait_data_sector(&mut self, spin_limit: u32) -> bool {
        let mut spins = spin_limit;
        loop {
            let flag = irq_flag();
            if flag == IRQ_DATA_READY {
                ack_irq(flag);
                return true;
            }
            if flag == IRQ_ERROR {
                let _ = read_response_fifo();
                ack_irq(flag);
                return false;
            }
            if flag != 0 {
                let _ = read_response_fifo();
                ack_irq(flag);
            }
            if spins == 0 {
                return false;
            }
            spins -= 1;
            core::hint::spin_loop();
        }
    }

    /// Get the CD-ROM drive status byte.
    #[doc(alias = "Getstat")]
    #[doc(alias = "CdlNop")]
    pub fn status(&mut self) -> Result<Response, CdError> {
        self.command(CMD_GETSTAT, &[])
    }

    /// Try to get the CD-ROM drive status byte.
    #[doc(alias = "Getstat")]
    pub fn try_status(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_GETSTAT, &[], spin_limit)
    }

    /// Set the CD-ROM controller mode byte.
    pub fn set_mode(&mut self, mode: u8) -> Result<Response, CdError> {
        self.command(CMD_SETMODE, &[mode])
    }

    /// Try to set the CD-ROM controller mode byte.
    pub fn try_set_mode(&mut self, mode: u8, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_SETMODE, &[mode], spin_limit)
    }

    /// Seek to a logical data-sector LBA (the command receives absolute BCD MSF).
    #[doc(alias = "Setloc")]
    #[doc(alias = "CdlSetloc")]
    pub fn try_set_target_lba(&mut self, lba: u32, spin_limit: u32) -> Option<Response> {
        let [minute, second, frame] = lba_to_bcd_msf(lba);
        self.try_command(CMD_SETLOC, &[minute, second, frame], spin_limit)
    }

    /// Begin a normal data-sector stream at the most recently selected location.
    #[doc(alias = "ReadN")]
    #[doc(alias = "CdlReadN")]
    pub fn try_start_reading(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_READN, &[], spin_limit)
    }

    /// Route CD-DA/XA output out of the CD-ROM controller.
    #[doc(alias = "Demute")]
    #[doc(alias = "CdlDemute")]
    pub fn unmute(&mut self) -> Result<Response, CdError> {
        self.command(CMD_DEMUTE, &[])
    }

    /// Try to route CD-DA/XA output out of the CD-ROM controller.
    #[doc(alias = "Demute")]
    pub fn try_unmute(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_DEMUTE, &[], spin_limit)
    }

    /// Mute CD-DA/XA output at the CD-ROM controller.
    pub fn mute(&mut self) -> Result<Response, CdError> {
        self.command(CMD_MUTE, &[])
    }

    /// Try to mute CD-DA/XA output at the CD-ROM controller.
    pub fn try_mute(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_MUTE, &[], spin_limit)
    }

    /// Start CD-DA playback at a 1-based track number.
    ///
    /// The number is relative to this program's own tracks; on a multi-program
    /// disc [`crate::disc_base`] shifts it past whatever came before.
    pub fn play_track(&mut self, track: u8) -> Result<Response, CdError> {
        self.command(
            CMD_PLAY,
            &[bin_to_bcd(crate::disc_base::shift_track(track))],
        )
    }

    /// Try to start CD-DA playback at a 1-based track number. Shifted like
    /// [`Self::play_track`].
    pub fn try_play_track(&mut self, track: u8, spin_limit: u32) -> Option<Response> {
        self.try_command(
            CMD_PLAY,
            &[bin_to_bcd(crate::disc_base::shift_track(track))],
            spin_limit,
        )
    }

    /// Pause CD-DA/read playback.
    pub fn pause(&mut self) -> Result<Response, CdError> {
        self.command(CMD_PAUSE, &[])
    }

    /// Try to pause CD-DA/read playback.
    pub fn try_pause(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_PAUSE, &[], spin_limit)
    }

    /// Try to pause CD-DA/read playback and wait for the completion IRQ.
    ///
    /// Unlike [`Self::try_stop`], this leaves the drive spun up, which makes it the
    /// right handoff before gameplay code starts issuing data-read commands.
    pub fn try_pause_until_complete(&mut self, spin_limit: u32) -> bool {
        try_command_until_complete_inner(CMD_PAUSE, &[], spin_limit)
    }

    /// Stop the CD-ROM motor/playback.
    pub fn stop(&mut self) -> Result<Response, CdError> {
        self.command(CMD_STOP, &[])
    }

    /// Try to stop the CD-ROM motor/playback.
    ///
    /// **This returns when the command is ACCEPTED, not when the drive is
    /// done.** On real hardware the motor then winds down for one to two
    /// seconds, during which GetStat reports no activity bits and data-read
    /// commands fail; an emulator answers instantly and hides the window
    /// (proven by a demo-disc burn whose every chain-load died this way). If
    /// data reads follow, either call [`Self::stop_and_settle`], or better, keep the
    /// drive spun up with [`Self::try_pause_until_complete`].
    pub fn try_stop(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_STOP, &[], spin_limit)
    }

    /// Stop playback and wait until the drive has genuinely gone quiet:
    /// GetStat reporting neither playing nor seeking on two consecutive polls,
    /// with a short pause between polls. Bounded by `max_polls` so a wedged
    /// drive cannot hang the caller; returns whether the drive settled.
    ///
    /// This is the safe prelude to issuing data-read commands after CD-DA.
    /// [`Self::try_pause_until_complete`] is the faster choice when the next reads
    /// are imminent, since it keeps the motor spinning.
    pub fn stop_and_settle(&mut self, spin_limit: u32, max_polls: u32) -> bool {
        let _ = self.try_stop(spin_limit);
        let mut settled = 0u8;
        for _ in 0..max_polls {
            // Pace the polls: on silicon each takes real time anyway, but an
            // emulator answers instantly and would burn the poll budget in
            // microseconds. Volatile MMIO reads cannot be optimized out.
            for _ in 0..20_000u32 {
                // SAFETY: 0x1F80_1800 is the CD-ROM controller's index/status register, byte-wide MMIO
                // on every PS1. Reading it has no side effects, so this is a pure delay.
                unsafe { core::ptr::read_volatile(0x1F80_1800 as *const u8) };
            }
            let quiet = match self.try_status(spin_limit) {
                Some(r) => r
                    .bytes()
                    .first()
                    .is_some_and(|s| s & (STAT_PLAYING | STAT_SEEKING) == 0),
                None => false,
            };
            settled = if quiet { settled + 1 } else { 0 };
            if settled >= 2 {
                return true;
            }
        }
        false
    }

    /// Get the current physical play position (CdlGetlocP). The 8-byte reply is
    /// `[Track(bcd), Index(raw), RMM, RSS, RSECT, AMM, ASS, ASECT]`; parse it with
    /// [`PlayPosition::parse`].
    #[doc(alias = "GetlocP")]
    #[doc(alias = "CdlGetlocP")]
    pub fn play_position(&mut self) -> Result<Response, CdError> {
        self.command(CMD_GETLOCP, &[])
    }

    /// Try to get the current physical play position, giving up after `spin_limit`.
    #[doc(alias = "GetlocP")]
    pub fn try_play_position(&mut self, spin_limit: u32) -> Option<Response> {
        self.try_command(CMD_GETLOCP, &[], spin_limit)
    }

    /// Send a command and wait for its SECOND response (the completion IRQ),
    /// not just the initial acknowledgement.
    ///
    /// Seek, read and init all acknowledge immediately and finish much later, so
    /// timing them against the ack measures command dispatch rather than the
    /// mechanical operation. Returns `false` if either response fails to arrive
    /// within `spin_limit` polls.
    pub fn try_command_until_complete(
        &mut self,
        command: u8,
        params: &[u8],
        spin_limit: u32,
    ) -> bool {
        try_command_until_complete_inner(command, params, spin_limit)
    }

    /// Program the drive's audio mixer (CD-DA and XA-ADPCM on their way to
    /// the SPU's CD input) and apply it. Volumes are 0..=0xFF with 0x80 as
    /// unity; `(0x80, 0, 0x80, 0)` is plain stereo. The SPU side still needs
    /// its CD input enabled and a CD volume (`psx-spu`).
    pub fn set_audio_mixer(
        &mut self,
        left_to_left: u8,
        left_to_right: u8,
        right_to_right: u8,
        right_to_left: u8,
    ) {
        select_index(2);
        write_byte(REG_PARAMETER, left_to_left);
        write_byte(REG_REQUEST_IRQ, left_to_right);
        select_index(3);
        write_byte(REG_COMMAND_RESPONSE, right_to_right);
        write_byte(REG_PARAMETER, right_to_left);
        // Apply the new volumes (index 3, register 3, bit 5), un-muting ADPCM.
        write_byte(REG_REQUEST_IRQ, 0x20);
        select_index(0);
    }
}

/// Register-level steps, for a driver that sequences commands itself, such as
/// the polled [`reader::SectorReader`].
///
/// Each is one controller access or a short fixed run of them, and each
/// leaves register index 0 selected, so the byte steps that follow need not
/// select it again.
impl Cd {
    /// Acknowledge every controller IRQ (writing `0x5F`, which also resets
    /// the parameter FIFO) and the CD-ROM bit of `I_STAT`.
    pub fn acknowledge_all_and_reset_parameters(&mut self) {
        ack_all_and_reset_parameters();
    }

    /// The controller's IRQ enable bits.
    pub fn irq_enable_mask(&mut self) -> u8 {
        irq_enable()
    }

    /// Set the controller's IRQ enable bits (five are defined).
    pub fn set_irq_enable_mask(&mut self, mask: u8) {
        set_irq_enable(mask);
    }

    /// Pop and discard up to `limit` response bytes. The FIFO is 16 bytes
    /// deep; the bound is for a controller wedged "not empty".
    pub fn drain_response_limited(&mut self, limit: u32) {
        drain_response_limited(limit);
    }

    /// Whether the data FIFO holds sector bytes to pop.
    pub fn is_data_fifo_ready(&mut self) -> bool {
        data_fifo_ready()
    }

    /// Wait for the parameter FIFO to have room, for at most `spins` more
    /// status reads. `false` on timeout.
    pub fn wait_parameter_room(&mut self, spins: u32) -> bool {
        wait_param_room_bounded(spins)
    }

    /// Empty the parameter FIFO.
    pub fn reset_parameter_fifo(&mut self) {
        clear_parameter_fifo();
    }

    /// Push one parameter byte. Wait for room first.
    pub fn send_parameter_byte(&mut self, byte: u8) {
        write_byte(REG_PARAMETER, byte);
    }

    /// Write the command byte, which starts the command.
    pub fn send_command_byte(&mut self, byte: u8) {
        write_byte(REG_COMMAND_RESPONSE, byte);
    }

    /// Pop one response byte.
    pub fn read_response_byte(&mut self) -> u8 {
        read_byte(REG_COMMAND_RESPONSE)
    }

    /// The status register: FIFO flags and the busy bit.
    pub fn status_register(&mut self) -> u8 {
        read_status()
    }

    /// Arm the data FIFO so the drive's buffered sector can be popped with
    /// [`read_data_byte`](Self::read_data_byte) once
    /// [`is_data_fifo_ready`](Self::is_data_fifo_ready) says it filled.
    #[doc(alias = "BFRD")]
    pub fn request_data(&mut self) {
        write_byte(REG_REQUEST_IRQ, REQUEST_DATA);
    }

    /// Drop the data request, which resets the data FIFO.
    pub fn clear_data_request(&mut self) {
        write_byte(REG_REQUEST_IRQ, 0);
    }

    /// Pop one byte of sector data.
    pub fn read_data_byte(&mut self) -> u8 {
        read_byte(REG_PARAMETER)
    }
}

/// The controller steps one polled command takes, so the order and the
/// timeouts can be tested against a fake.
trait CommandIo {
    fn begin(&mut self) -> u8;
    fn wait_param_room(&mut self, spins: u32) -> bool;
    fn write_param(&mut self, value: u8);
    fn write_command(&mut self, command: u8);
    fn wait_irq(&mut self, expected: u8, spins: u32) -> Result<u8, CdError>;
    fn finish(&mut self, irq_enable: u8, irq: u8) -> Response;
    /// Leave the controller clean after a drive error: drain, acknowledge
    /// everything and restore the IRQ enable.
    fn recover(&mut self, irq_enable: u8);
    /// Give the IRQ enable back without touching the response side.
    fn restore_enable(&mut self, irq_enable: u8);
}

struct Mmio;

impl CommandIo for Mmio {
    fn begin(&mut self) -> u8 {
        begin_polled_command()
    }
    fn wait_param_room(&mut self, spins: u32) -> bool {
        wait_param_room_bounded(spins)
    }
    fn write_param(&mut self, value: u8) {
        write_byte(REG_PARAMETER, value);
    }
    fn write_command(&mut self, command: u8) {
        write_byte(REG_COMMAND_RESPONSE, command);
    }
    fn wait_irq(&mut self, expected: u8, spins: u32) -> Result<u8, CdError> {
        wait_irq_flag(expected, spins, irq_flag, |flag| {
            let _ = read_response_fifo();
            ack_irq(flag);
        })
    }
    fn finish(&mut self, irq_enable: u8, irq: u8) -> Response {
        finish_polled_command(irq_enable, irq)
    }
    fn recover(&mut self, irq_enable: u8) {
        finish_failed_polled_command(irq_enable);
    }
    fn restore_enable(&mut self, irq_enable: u8) {
        restore_irq_enable(irq_enable);
        select_index(0);
    }
}

fn run_command(
    io: &mut impl CommandIo,
    command: u8,
    params: &[u8],
    spin_limit: u32,
) -> Result<Response, CdError> {
    let irq_enable = io.begin();
    for &param in params {
        if !io.wait_param_room(spin_limit) {
            // Nothing was dispatched, so the IRQ output can be given back.
            io.restore_enable(irq_enable);
            return Err(CdError::Timeout);
        }
        io.write_param(param);
    }
    io.write_command(command);
    match io.wait_irq(IRQ_ACK, spin_limit) {
        Ok(irq) => Ok(io.finish(irq_enable, irq)),
        Err(CdError::DriveError) => {
            io.recover(irq_enable);
            Err(CdError::DriveError)
        }
        // A command is in flight: leave IRQ output masked so its late ACK
        // cannot interrupt a polling caller.
        Err(CdError::Timeout) => Err(CdError::Timeout),
    }
}

/// A drive error reported while polling for the next streamed sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectorPollError;

trait SectorPollIo {
    fn flag(&mut self) -> u8;
    fn drain(&mut self);
    fn acknowledge(&mut self, flag: u8, reset: bool);
    fn data_ready(&mut self) -> bool;
}
struct SectorPollMmio;
impl SectorPollIo for SectorPollMmio {
    #[inline]
    fn flag(&mut self) -> u8 {
        irq_flag()
    }
    #[inline]
    fn drain(&mut self) {
        drain_response_limited(SECTOR_POLL_DRAIN_LIMIT);
    }
    #[inline]
    fn acknowledge(&mut self, flag: u8, reset: bool) {
        if reset {
            ack_all_and_reset_parameters();
        } else {
            ack_irq(flag);
        }
    }
    #[inline]
    fn data_ready(&mut self) -> bool {
        data_fifo_ready()
    }
}
#[inline]
fn poll_sector(io: &mut impl SectorPollIo) -> Result<bool, SectorPollError> {
    match io.flag() {
        IRQ_DATA_READY => Ok(true),
        IRQ_ERROR => {
            io.drain();
            io.acknowledge(IRQ_ACK_ALL, true);
            Err(SectorPollError)
        }
        flag @ (IRQ_COMPLETE | IRQ_ACK | IRQ_DATA_END) => {
            io.drain();
            io.acknowledge(flag, false);
            Ok(false)
        }
        _ => Ok(io.data_ready()),
    }
}

/// Data-track LBA to the absolute BCD `[minute, second, frame]` Setloc takes.
/// LBA 0 is 00:02:00: the 150-sector lead-in puts it two seconds in. Minutes
/// clamp at 99.
pub const fn lba_to_bcd_msf(lba: u32) -> [u8; 3] {
    let absolute = lba.saturating_add(150);
    let raw_minute = absolute / (60 * 75);
    let minute = if raw_minute > 99 {
        99
    } else {
        raw_minute as u8
    };
    let second = ((absolute / 75) % 60) as u8;
    let frame = (absolute % 75) as u8;
    [bin_to_bcd(minute), bin_to_bcd(second), bin_to_bcd(frame)]
}

/// Convert binary `0..=99` to BCD for CD-ROM command parameters.
pub const fn bin_to_bcd(v: u8) -> u8 {
    let v = if v > 99 { 99 } else { v };
    ((v / 10) << 4) | (v % 10)
}

/// Convert a BCD byte back to binary. Inverse of [`bin_to_bcd`] for well-formed
/// input; nibbles above 9 are not normalised (the drive never emits them).
pub const fn bcd_to_bin(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

/// Decoded CdlGetlocP reply. The relative MSF (`relative_*`) is elapsed time
/// into the current track, independent of the pregap, which makes it the source
/// of truth for a music/song clock. The absolute MSF (`absolute_*`) is the raw
/// disc position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlayPosition {
    /// 1-based track number the drive is currently in.
    pub track: u8,
    /// Track index (raw): 0 = pregap, 1 = program area.
    pub index: u8,
    /// Minutes elapsed into the current track.
    pub relative_min: u8,
    /// Seconds elapsed into the current track (0..=59).
    pub relative_sec: u8,
    /// MSF frames elapsed into the current track (0..=74, 75 per second).
    pub relative_frame: u8,
    /// Absolute disc position, minutes.
    pub absolute_min: u8,
    /// Absolute disc position, seconds (0..=59).
    pub absolute_sec: u8,
    /// Absolute disc position, MSF frames (0..=74).
    pub absolute_frame: u8,
}

impl PlayPosition {
    /// Parse a GetlocP [`Response`]. Returns `None` if fewer than 8 bytes came
    /// back (drive not ready / no disc). Track and MSF fields are BCD; index is
    /// raw.
    pub fn parse(resp: &Response) -> Option<Self> {
        let b = resp.bytes();
        if b.len() < 8 {
            return None;
        }
        Some(PlayPosition {
            track: bcd_to_bin(b[0]),
            index: b[1],
            relative_min: bcd_to_bin(b[2]),
            relative_sec: bcd_to_bin(b[3]),
            relative_frame: bcd_to_bin(b[4]),
            absolute_min: bcd_to_bin(b[5]),
            absolute_sec: bcd_to_bin(b[6]),
            absolute_frame: bcd_to_bin(b[7]),
        })
    }

    /// Elapsed sectors into the current track (75 sectors per second).
    pub fn relative_sectors(&self) -> u32 {
        (self.relative_min as u32 * 60 + self.relative_sec as u32) * 75 + self.relative_frame as u32
    }

    /// Elapsed milliseconds into the current track. This is the song clock feed.
    pub fn relative_millis(&self) -> u32 {
        self.relative_sectors() * 1000 / 75
    }
}

fn begin_polled_command() -> u8 {
    let irq_enable = irq_enable();
    set_irq_enable(0);
    ack_irq(IRQ_ACK_ALL);
    select_index(0);
    drain_response_fifo();
    clear_parameter_fifo();
    irq_enable
}

fn finish_polled_command(irq_enable: u8, irq: u8) -> Response {
    let response = read_response_fifo();
    ack_irq(irq);
    restore_irq_enable(irq_enable);
    select_index(0);
    response
}

/// Poll `flag` until it reads `expected`, at most `spins` more times after the
/// first read. A drive error (INT5) ends the wait at once; any other nonzero
/// flag is a stale response, handed to `discard` and polled past.
fn wait_irq_flag(
    expected: u8,
    mut spins: u32,
    mut flag: impl FnMut() -> u8,
    mut discard: impl FnMut(u8),
) -> Result<u8, CdError> {
    loop {
        let irq = flag();
        if irq == expected {
            return Ok(irq);
        }
        if irq == IRQ_ERROR {
            return Err(CdError::DriveError);
        }
        if irq != 0 {
            discard(irq);
        }
        if spins == 0 {
            return Err(CdError::Timeout);
        }
        spins -= 1;
        core::hint::spin_loop();
    }
}

fn wait_irq_bounded(expected: u8, spins: u32) -> Option<u8> {
    Mmio.wait_irq(expected, spins).ok()
}

fn try_command_until_complete_inner(command: u8, params: &[u8], spin_limit: u32) -> bool {
    let irq_enable = begin_polled_command();
    select_index(0);
    for &param in params {
        if !wait_param_room_bounded(spin_limit) {
            finish_failed_polled_command(irq_enable);
            return false;
        }
        write_byte(REG_PARAMETER, param);
    }
    write_byte(REG_COMMAND_RESPONSE, command);

    let ok = wait_ack_then_complete(spin_limit);
    drain_response_fifo();
    ack_irq(IRQ_ACK_ALL);
    restore_irq_enable(irq_enable);
    select_index(0);
    ok
}

fn wait_ack_then_complete(spin_limit: u32) -> bool {
    if wait_irq_bounded(IRQ_ACK, spin_limit).is_none() {
        return false;
    }
    drain_response_fifo();
    ack_irq(IRQ_ACK);

    if wait_irq_bounded(IRQ_COMPLETE, spin_limit).is_none() {
        return false;
    }
    drain_response_fifo();
    ack_irq(IRQ_COMPLETE);
    true
}

fn finish_failed_polled_command(irq_enable: u8) {
    drain_response_fifo();
    ack_irq(IRQ_ACK_ALL);
    restore_irq_enable(irq_enable);
    select_index(0);
}

fn read_response_fifo() -> Response {
    select_index(0);

    let mut bytes = [0u8; 16];
    let mut len = 0;
    while read_status() & STATUS_RESPONSE_NOT_EMPTY != 0 && len < bytes.len() {
        bytes[len] = read_byte(REG_COMMAND_RESPONSE);
        len += 1;
    }
    Response { bytes, len }
}

fn drain_response_fifo() {
    let _ = read_response_fifo();
}

fn wait_param_room_bounded(mut spins: u32) -> bool {
    while read_status() & STATUS_PARAM_NOT_FULL == 0 {
        if spins == 0 {
            return false;
        }
        spins -= 1;
        core::hint::spin_loop();
    }
    true
}

/// Pop and discard up to `limit` response bytes.
fn drain_response_limited(limit: u32) {
    select_index(0);
    let mut drained = 0;
    while read_status() & STATUS_RESPONSE_NOT_EMPTY != 0 && drained < limit {
        let _ = read_byte(REG_COMMAND_RESPONSE);
        drained += 1;
    }
}

/// Acknowledge every controller IRQ and reset the parameter FIFO, then the
/// CD-ROM bit of I_STAT.
fn ack_all_and_reset_parameters() {
    select_index(1);
    write_byte(REG_REQUEST_IRQ, IRQ_ACK_ALL | IRQ_PARAM_FIFO_RESET);
    irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    select_index(0);
}

fn data_fifo_ready() -> bool {
    select_index(0);
    read_status() & STATUS_DATA_FIFO_NOT_EMPTY != 0
}

fn clear_parameter_fifo() {
    select_index(1);
    write_byte(REG_REQUEST_IRQ, IRQ_PARAM_FIFO_RESET);
    select_index(0);
}

fn ack_irq(bits: u8) {
    select_index(1);
    write_byte(REG_REQUEST_IRQ, bits & IRQ_ACK_ALL);
    irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    select_index(0);
}

fn irq_flag() -> u8 {
    select_index(1);
    let flag = read_byte(REG_REQUEST_IRQ) & IRQ_ACK_ALL;
    select_index(0);
    flag
}

fn irq_enable() -> u8 {
    select_index(0);
    let enable = read_byte(REG_REQUEST_IRQ) & IRQ_ACK_ALL;
    select_index(0);
    enable
}

fn restore_irq_enable(enable: u8) {
    set_irq_enable(enable);
}

fn set_irq_enable(enable: u8) {
    select_index(1);
    write_byte(REG_PARAMETER, enable & IRQ_ACK_ALL);
    select_index(0);
}

fn read_status() -> u8 {
    read_byte(REG_INDEX)
}

fn select_index(index: u8) {
    write_byte(REG_INDEX, index & 0x03);
}

fn read_byte(addr: u32) -> u8 {
    // SAFETY: fixed CD-ROM MMIO register read.
    unsafe { read_u8(addr) }
}

fn write_byte(addr: u32, value: u8) {
    // SAFETY: fixed CD-ROM MMIO register write.
    unsafe { write_u8(addr, value) }
}

/// A token for a deprecated forwarder that never took one.
fn steal() -> Cd {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `crate::periph`), and the old free functions never took one.
    unsafe { Cd::steal() }
}

/// Moved to [`Cd::command`].
#[deprecated(note = "use `Cd::command` with the `Cd` token")]
#[inline(always)]
pub fn command(command: u8, params: &[u8]) -> Result<Response, CdError> {
    steal().command(command, params)
}

/// Moved to [`Cd::command_within`].
#[deprecated(note = "use `Cd::command_within` with the `Cd` token")]
#[inline(always)]
pub fn command_within(command: u8, params: &[u8], spin_limit: u32) -> Result<Response, CdError> {
    steal().command_within(command, params, spin_limit)
}

/// Moved to [`Cd::try_command`].
#[deprecated(note = "use `Cd::try_command` with the `Cd` token")]
#[inline(always)]
pub fn try_command(command: u8, params: &[u8], spin_limit: u32) -> Option<Response> {
    steal().try_command(command, params, spin_limit)
}

/// Moved to [`Cd::irq_flag_value`].
#[deprecated(note = "use `Cd::irq_flag_value` with the `Cd` token")]
#[inline(always)]
pub fn irq_flag_value() -> u8 {
    steal().irq_flag_value()
}

/// Moved to [`Cd::acknowledge_irq`].
#[deprecated(note = "use `Cd::acknowledge_irq` with the `Cd` token")]
#[inline(always)]
pub fn acknowledge_irq(bits: u8) {
    steal().acknowledge_irq(bits)
}

/// Moved to [`Cd::discard_response`].
#[deprecated(note = "use `Cd::discard_response` with the `Cd` token")]
#[inline(always)]
pub fn discard_response() {
    steal().discard_response()
}

/// Moved to [`Cd::dispatch_command`].
#[deprecated(note = "use `Cd::dispatch_command` with the `Cd` token")]
#[inline(always)]
pub fn dispatch_command(command: u8, params: &[u8], spin_limit: u32) -> Option<u8> {
    steal().dispatch_command(command, params, spin_limit)
}

/// Moved to [`Cd::restore_irq_output`].
#[deprecated(note = "use `Cd::restore_irq_output` with the `Cd` token")]
#[inline(always)]
pub fn restore_irq_output(saved: u8) {
    steal().restore_irq_output(saved)
}

/// Moved to [`Cd::poll_data_sector`].
#[deprecated(note = "use `Cd::poll_data_sector` with the `Cd` token")]
#[inline(always)]
pub fn poll_data_sector() -> Result<bool, SectorPollError> {
    steal().poll_data_sector()
}

/// Moved to [`Cd::try_wait_data_sector`].
#[deprecated(note = "use `Cd::try_wait_data_sector` with the `Cd` token")]
#[inline(always)]
pub fn try_wait_data_sector(spin_limit: u32) -> bool {
    steal().try_wait_data_sector(spin_limit)
}

/// Moved to [`Cd::status`].
#[deprecated(note = "use `Cd::status` with the `Cd` token")]
#[inline(always)]
pub fn status() -> Result<Response, CdError> {
    steal().status()
}

/// Moved to [`Cd::try_status`].
#[deprecated(note = "use `Cd::try_status` with the `Cd` token")]
#[inline(always)]
pub fn try_status(spin_limit: u32) -> Option<Response> {
    steal().try_status(spin_limit)
}

/// Moved to [`Cd::set_mode`].
#[deprecated(note = "use `Cd::set_mode` with the `Cd` token")]
#[inline(always)]
pub fn set_mode(mode: u8) -> Result<Response, CdError> {
    steal().set_mode(mode)
}

/// Moved to [`Cd::try_set_mode`].
#[deprecated(note = "use `Cd::try_set_mode` with the `Cd` token")]
#[inline(always)]
pub fn try_set_mode(mode: u8, spin_limit: u32) -> Option<Response> {
    steal().try_set_mode(mode, spin_limit)
}

/// Moved to [`Cd::try_set_target_lba`].
#[deprecated(note = "use `Cd::try_set_target_lba` with the `Cd` token")]
#[inline(always)]
pub fn try_set_target_lba(lba: u32, spin_limit: u32) -> Option<Response> {
    steal().try_set_target_lba(lba, spin_limit)
}

/// Moved to [`Cd::try_start_reading`].
#[deprecated(note = "use `Cd::try_start_reading` with the `Cd` token")]
#[inline(always)]
pub fn try_start_reading(spin_limit: u32) -> Option<Response> {
    steal().try_start_reading(spin_limit)
}

/// Moved to [`Cd::unmute`].
#[deprecated(note = "use `Cd::unmute` with the `Cd` token")]
#[inline(always)]
pub fn unmute() -> Result<Response, CdError> {
    steal().unmute()
}

/// Moved to [`Cd::try_unmute`].
#[deprecated(note = "use `Cd::try_unmute` with the `Cd` token")]
#[inline(always)]
pub fn try_unmute(spin_limit: u32) -> Option<Response> {
    steal().try_unmute(spin_limit)
}

/// Moved to [`Cd::mute`].
#[deprecated(note = "use `Cd::mute` with the `Cd` token")]
#[inline(always)]
pub fn mute() -> Result<Response, CdError> {
    steal().mute()
}

/// Moved to [`Cd::try_mute`].
#[deprecated(note = "use `Cd::try_mute` with the `Cd` token")]
#[inline(always)]
pub fn try_mute(spin_limit: u32) -> Option<Response> {
    steal().try_mute(spin_limit)
}

/// Moved to [`Cd::play_track`].
#[deprecated(note = "use `Cd::play_track` with the `Cd` token")]
#[inline(always)]
pub fn play_track(track: u8) -> Result<Response, CdError> {
    steal().play_track(track)
}

/// Moved to [`Cd::try_play_track`].
#[deprecated(note = "use `Cd::try_play_track` with the `Cd` token")]
#[inline(always)]
pub fn try_play_track(track: u8, spin_limit: u32) -> Option<Response> {
    steal().try_play_track(track, spin_limit)
}

/// Moved to [`Cd::pause`].
#[deprecated(note = "use `Cd::pause` with the `Cd` token")]
#[inline(always)]
pub fn pause() -> Result<Response, CdError> {
    steal().pause()
}

/// Moved to [`Cd::try_pause`].
#[deprecated(note = "use `Cd::try_pause` with the `Cd` token")]
#[inline(always)]
pub fn try_pause(spin_limit: u32) -> Option<Response> {
    steal().try_pause(spin_limit)
}

/// Moved to [`Cd::try_pause_until_complete`].
#[deprecated(note = "use `Cd::try_pause_until_complete` with the `Cd` token")]
#[inline(always)]
pub fn try_pause_until_complete(spin_limit: u32) -> bool {
    steal().try_pause_until_complete(spin_limit)
}

/// Moved to [`Cd::stop`].
#[deprecated(note = "use `Cd::stop` with the `Cd` token")]
#[inline(always)]
pub fn stop() -> Result<Response, CdError> {
    steal().stop()
}

/// Moved to [`Cd::try_stop`].
#[deprecated(note = "use `Cd::try_stop` with the `Cd` token")]
#[inline(always)]
pub fn try_stop(spin_limit: u32) -> Option<Response> {
    steal().try_stop(spin_limit)
}

/// Moved to [`Cd::stop_and_settle`].
#[deprecated(note = "use `Cd::stop_and_settle` with the `Cd` token")]
#[inline(always)]
pub fn stop_and_settle(spin_limit: u32, max_polls: u32) -> bool {
    steal().stop_and_settle(spin_limit, max_polls)
}

/// Moved to [`Cd::play_position`].
#[deprecated(note = "use `Cd::play_position` with the `Cd` token")]
#[inline(always)]
pub fn play_position() -> Result<Response, CdError> {
    steal().play_position()
}

/// Moved to [`Cd::try_play_position`].
#[deprecated(note = "use `Cd::try_play_position` with the `Cd` token")]
#[inline(always)]
pub fn try_play_position(spin_limit: u32) -> Option<Response> {
    steal().try_play_position(spin_limit)
}

/// Moved to [`Cd::try_command_until_complete`].
#[deprecated(note = "use `Cd::try_command_until_complete` with the `Cd` token")]
#[inline(always)]
pub fn try_command_until_complete(command: u8, params: &[u8], spin_limit: u32) -> bool {
    steal().try_command_until_complete(command, params, spin_limit)
}

/// Moved to [`Cd::set_audio_mixer`].
#[deprecated(note = "use `Cd::set_audio_mixer` with the `Cd` token")]
#[inline(always)]
pub fn set_audio_mixer(left_to_left: u8, left_to_right: u8, right_to_right: u8, right_to_left: u8) {
    steal().set_audio_mixer(left_to_left, left_to_right, right_to_right, right_to_left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_probe_leaves_int1_and_orders_other_acknowledgements() {
        struct Fake {
            flag: u8,
            ready: bool,
            events: [u8; 4],
            len: usize,
        }
        impl Fake {
            fn event(&mut self, e: u8) {
                self.events[self.len] = e;
                self.len += 1;
            }
        }
        impl SectorPollIo for Fake {
            fn flag(&mut self) -> u8 {
                self.event(1);
                self.flag
            }
            fn drain(&mut self) {
                self.event(2);
            }
            fn acknowledge(&mut self, flag: u8, reset: bool) {
                assert_eq!(flag, if reset { 31 } else { self.flag });
                self.event(if reset { 4 } else { 3 });
            }
            fn data_ready(&mut self) -> bool {
                self.event(5);
                self.ready
            }
        }
        for flag in 0..=5 {
            for ready in [false, true] {
                let mut io = Fake {
                    flag,
                    ready,
                    events: [0; 4],
                    len: 0,
                };
                let result = poll_sector(&mut io);
                let expected = match flag {
                    1 => Ok(true),
                    5 => Err(SectorPollError),
                    2..=4 => Ok(false),
                    _ => Ok(ready),
                };
                assert_eq!(result, expected);
                let events: &[u8] = match flag {
                    1 => &[1],
                    5 => &[1, 2, 4],
                    2..=4 => &[1, 2, 3],
                    _ => &[1, 5],
                };
                assert_eq!(&io.events[..io.len], events);
            }
        }
    }

    /// Records the controller steps a command takes and answers each wait from
    /// a script.
    struct FakeIo {
        room: bool,
        irq: Result<u8, CdError>,
        events: [u8; 12],
        len: usize,
        spins_seen: [u32; 4],
        spins_len: usize,
    }

    impl FakeIo {
        fn new(room: bool, irq: Result<u8, CdError>) -> Self {
            FakeIo {
                room,
                irq,
                events: [0; 12],
                len: 0,
                spins_seen: [0; 4],
                spins_len: 0,
            }
        }
        fn event(&mut self, e: u8) {
            self.events[self.len] = e;
            self.len += 1;
        }
        fn events(&self) -> &[u8] {
            &self.events[..self.len]
        }
        fn spins(&mut self, spins: u32) {
            self.spins_seen[self.spins_len] = spins;
            self.spins_len += 1;
        }
    }

    // Event codes: begin, param room, param byte, command byte, wait irq,
    // finish, recover, restore enable.
    const BEGIN: u8 = 1;
    const ROOM: u8 = 2;
    const PARAM: u8 = 3;
    const COMMAND: u8 = 4;
    const WAIT: u8 = 5;
    const FINISH: u8 = 6;
    const RECOVER: u8 = 7;
    const RESTORE: u8 = 8;

    impl CommandIo for FakeIo {
        fn begin(&mut self) -> u8 {
            self.event(BEGIN);
            0x1F
        }
        fn wait_param_room(&mut self, spins: u32) -> bool {
            self.event(ROOM);
            self.spins(spins);
            self.room
        }
        fn write_param(&mut self, _value: u8) {
            self.event(PARAM);
        }
        fn write_command(&mut self, _command: u8) {
            self.event(COMMAND);
        }
        fn wait_irq(&mut self, _expected: u8, spins: u32) -> Result<u8, CdError> {
            self.event(WAIT);
            self.spins(spins);
            self.irq
        }
        fn finish(&mut self, _irq_enable: u8, _irq: u8) -> Response {
            self.event(FINISH);
            response_from(&[0x02])
        }
        fn recover(&mut self, _irq_enable: u8) {
            self.event(RECOVER);
        }
        fn restore_enable(&mut self, _irq_enable: u8) {
            self.event(RESTORE);
        }
    }

    #[test]
    fn a_full_parameter_fifo_times_out_without_writing_the_parameter() {
        // The wait used to ignore its own timeout and write the byte anyway.
        let mut io = FakeIo::new(false, Ok(IRQ_ACK));
        let result = run_command(&mut io, CMD_SETLOC, &[0x00, 0x02, 0x00], 77);
        assert_eq!(result, Err(CdError::Timeout));
        assert_eq!(io.events(), &[BEGIN, ROOM, RESTORE]);
    }

    #[test]
    fn a_command_nobody_acknowledges_times_out_and_leaves_irq_output_masked() {
        let mut io = FakeIo::new(true, Err(CdError::Timeout));
        let result = run_command(&mut io, CMD_GETSTAT, &[], DEFAULT_COMMAND_SPINS);
        assert_eq!(result, Err(CdError::Timeout));
        // No finish, recover or restore: the late ACK stays masked.
        assert_eq!(io.events(), &[BEGIN, COMMAND, WAIT]);
        assert_eq!(&io.spins_seen[..io.spins_len], &[DEFAULT_COMMAND_SPINS]);
    }

    #[test]
    fn a_drive_error_is_reported_and_the_controller_cleaned_up() {
        let mut io = FakeIo::new(true, Err(CdError::DriveError));
        let result = run_command(&mut io, CMD_PLAY, &[0x01], 10);
        assert_eq!(result, Err(CdError::DriveError));
        assert_eq!(io.events(), &[BEGIN, ROOM, PARAM, COMMAND, WAIT, RECOVER]);
    }

    #[test]
    fn an_acknowledged_command_returns_its_response() {
        let mut io = FakeIo::new(true, Ok(IRQ_ACK));
        let result = run_command(&mut io, CMD_SETLOC, &[1, 2, 3], 10).unwrap();
        assert_eq!(result.bytes(), &[0x02]);
        assert_eq!(
            io.events(),
            &[BEGIN, ROOM, PARAM, ROOM, PARAM, ROOM, PARAM, COMMAND, WAIT, FINISH]
        );
    }

    #[test]
    fn the_flag_wait_discards_stale_responses_and_stops_at_its_budget() {
        // Stale INT2 then INT3: the stale one is discarded, INT3 ends the wait.
        let script = [0u8, 2, 3];
        let mut next = 0;
        let mut discarded = [0u8; 4];
        let mut dropped = 0;
        let got = wait_irq_flag(
            IRQ_ACK,
            10,
            || {
                let flag = script[next];
                next += 1;
                flag
            },
            |flag| {
                discarded[dropped] = flag;
                dropped += 1;
            },
        );
        assert_eq!(got, Ok(IRQ_ACK));
        assert_eq!(&discarded[..dropped], &[2]);

        // Silence: one read plus `spins` more, then Timeout.
        let mut reads = 0u32;
        let got = wait_irq_flag(
            IRQ_ACK,
            5,
            || {
                reads += 1;
                0
            },
            |_| {},
        );
        assert_eq!(got, Err(CdError::Timeout));
        assert_eq!(reads, 6);

        // INT5 ends it at once, whatever budget is left.
        let got = wait_irq_flag(IRQ_ACK, 1000, || IRQ_ERROR, |_| {});
        assert_eq!(got, Err(CdError::DriveError));
    }

    #[test]
    fn bcd_clamps_to_two_digits() {
        assert_eq!(bin_to_bcd(2), 0x02);
        assert_eq!(bin_to_bcd(42), 0x42);
        assert_eq!(bin_to_bcd(100), 0x99);
    }

    #[test]
    fn bcd_to_bin_roundtrips() {
        for v in 0u8..=99 {
            assert_eq!(bcd_to_bin(bin_to_bcd(v)), v);
        }
    }

    #[test]
    fn lba_to_msf_includes_the_lead_in() {
        assert_eq!(lba_to_bcd_msf(0), [0x00, 0x02, 0x00]);
        assert_eq!(lba_to_bcd_msf(424), [0x00, 0x07, 0x49]);
        assert_eq!(lba_to_bcd_msf(992), [0x00, 0x15, 0x17]);
    }

    fn response_from(bytes: &[u8]) -> Response {
        let mut buf = [0u8; 16];
        buf[..bytes.len()].copy_from_slice(bytes);
        Response {
            bytes: buf,
            len: bytes.len(),
        }
    }

    #[test]
    fn play_position_parses_and_converts() {
        // Track 2, index 1, relative 01:23:45 (min:sec:frame), absolute 04:56:00.
        let resp = response_from(&[
            bin_to_bcd(2),
            0x01,
            bin_to_bcd(1),
            bin_to_bcd(23),
            bin_to_bcd(45),
            bin_to_bcd(4),
            bin_to_bcd(56),
            bin_to_bcd(0),
        ]);
        let p = PlayPosition::parse(&resp).unwrap();
        assert_eq!(p.track, 2);
        assert_eq!(p.index, 1);
        assert_eq!(
            (p.relative_min, p.relative_sec, p.relative_frame),
            (1, 23, 45)
        );
        // (1*60 + 23) * 75 + 45 = 6270 sectors.
        assert_eq!(p.relative_sectors(), 6270);
        // 6270 * 1000 / 75 = 83600 ms.
        assert_eq!(p.relative_millis(), 83_600);
    }

    #[test]
    fn play_position_rejects_short_response() {
        assert!(PlayPosition::parse(&response_from(&[0x02, 0x01, 0x00])).is_none());
    }
}
