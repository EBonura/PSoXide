//! CD-ROM controller.
//!
//! **Full implementation**, not a stub -- the plan is to carry this
//! from BIOS-boot-without-disc (Milestone B) all the way through
//! real disc reads (Milestone D onward) and XA audio (Milestone F).
//! The file will grow by phase:
//!
//! - **6a (this file)**: register infrastructure -- the index-based
//!   MMIO dispatch, the three FIFOs (parameter, response, data),
//!   the IRQ flag/mask register model, the raw status byte.
//! - **6b**: command dispatcher with async-response scheduling.
//!   Commands queue first- and second-response events at specific
//!   cycle offsets; `tick` fires them when their time comes.
//! - **6c**: core commands that appear in every BIOS boot --
//!   Sync / GetStat / Init / Demute / GetID / Pause.
//! - **6d**: disc-present commands -- SetLoc, SeekL/SeekP, ReadN/ReadS,
//!   sector data streaming into the data FIFO + DMA channel 3.
//! - **6e**: ISO9660 filesystem hook from `psx-iso`.
//! - **6f**: audio plumbing -- volume matrix, XA decode (deferred
//!   to Milestone F when it actually matters).
//!
//! **Reference**: nocash PSX-SPX "CDROM Drive" and
//! `pcsx-redux/src/core/cdrom.cc`. Status-byte bits + IRQ types +
//! index semantics all follow those two.
//!
//! This module is parity-safe at the register level -- software that
//! reads status / queues parameters / pops responses sees the values
//! Redux would return. Command side effects (seeking, reading) start
//! landing in 6b onwards.
//!
//! ## Provenance
//!
//! Portions of this module are parity-matched against, and in places
//! derived from, PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later. Points of
//! correspondence are flagged inline with `Redux` references. PSoXide is
//! released under GPL-2.0-or-later in part to honor this lineage; see
//! `LICENSE` and `docs/license-audit.md`.

use std::collections::VecDeque;

use psx_iso::{bcd_to_bin, msf_to_lba, Disc};

mod timing;
use timing::*;

/// Base MMIO address -- the whole controller fits in 4 bytes at
/// `0x1F80_1800..=0x1F80_1803`.
pub const BASE: u32 = 0x1F80_1800;
/// Range end (exclusive) -- `BASE + 4`.
pub const END: u32 = 0x1F80_1804;

/// Status-byte bits (read from `0x1F80_1800` at any index).
#[allow(dead_code)]
pub mod status_bit {
    /// Index (low 2 bits) -- selects which sub-register is visible at
    /// `0x1F80_1801..=0x1F80_1803`. Written via `0x1F80_1800`.
    pub const INDEX_MASK: u8 = 0b0000_0011;
    /// ADPCM-decoder busy.
    pub const ADPCM_BUSY: u8 = 1 << 2;
    /// Parameter FIFO is empty (room for writes).
    pub const PARAM_FIFO_EMPTY: u8 = 1 << 3;
    /// Parameter FIFO is *not* full (software checks this before push).
    pub const PARAM_FIFO_NOT_FULL: u8 = 1 << 4;
    /// Response FIFO is *not* empty (something to pop).
    pub const RESPONSE_FIFO_NOT_EMPTY: u8 = 1 << 5;
    /// Data FIFO is *not* empty (sector bytes available).
    pub const DATA_FIFO_NOT_EMPTY: u8 = 1 << 6;
    /// A command is in flight -- cleared when the first response arrives.
    pub const TRANSMISSION_BUSY: u8 = 1 << 7;
}

/// Interrupt types (value written to `0x1F80_1803 idx=1` and visible
/// via the IRQ-flag register). Only values 1..=5 are meaningful;
/// value 0 means "no interrupt" and is what the BIOS writes to ack.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IrqType {
    /// No interrupt -- canonical cleared state.
    None = 0,
    /// Async sector-data-ready -- fires for each sector during ReadN/S.
    DataReady = 1,
    /// Second response -- completion of a command whose 1st response
    /// indicated the action would take time (seek, read, init).
    Complete = 2,
    /// First response -- command accepted, ~50 k cycles after the write.
    Acknowledge = 3,
    /// Fourth response -- end of data for Play/ReadS/ReadN on bounds.
    DataEnd = 4,
    /// Error response -- command rejected, disc error, etc.
    Error = 5,
}

impl IrqType {
    #[allow(dead_code)]
    fn from_u8(v: u8) -> Self {
        match v & 0x7 {
            1 => IrqType::DataReady,
            2 => IrqType::Complete,
            3 => IrqType::Acknowledge,
            4 => IrqType::DataEnd,
            5 => IrqType::Error,
            _ => IrqType::None,
        }
    }
}

/// Drive-status byte (a.k.a. "stat" -- the first byte of every
/// command response). Separate from the MMIO status byte above.
///
/// On cold boot with no disc, `SHELL_OPEN` is set and `MOTOR_ON`
/// is clear. Once the BIOS `Init` runs, the motor spins up; if a
/// disc is present, `SHELL_OPEN` clears and `MOTOR_ON` sets.
#[allow(dead_code)]
pub mod drive_status_bit {
    /// Error bit (set on invalid command, disc error).
    pub const ERROR: u8 = 1 << 0;
    /// Spindle motor is on (disc spinning).
    pub const MOTOR_ON: u8 = 1 << 1;
    /// Last seek failed.
    pub const SEEK_ERROR: u8 = 1 << 2;
    /// GetID detected a disc mismatch / unlicensed disc.
    pub const ID_ERROR: u8 = 1 << 3;
    /// Drive shell is open (no disc / cover lifted).
    pub const SHELL_OPEN: u8 = 1 << 4;
    /// Drive is currently reading sectors (ReadN / ReadS).
    pub const READING: u8 = 1 << 5;
    /// Drive is currently seeking.
    pub const SEEKING: u8 = 1 << 6;
    /// Drive is currently playing CD-DA audio.
    pub const PLAYING: u8 = 1 << 7;
}

/// Depth of both FIFOs in the controller. Real hardware is 16 bytes
/// for parameter / response, 2352 bytes for the sector data buffer.
/// Sector buffers the CD controller holds, counting the one software is
/// draining. A read that falls further behind than this loses sectors.
const SECTOR_BUFFERS: usize = 8;
const PARAM_FIFO_DEPTH: usize = 16;
const RESPONSE_FIFO_DEPTH: usize = 16;
const CDDA_BYTES_PER_SAMPLE: usize = 4;
const CDDA_SAMPLES_PER_SECTOR: usize = psx_iso::SECTOR_BYTES / CDDA_BYTES_PER_SAMPLE;

// Sector-read cycles are derived per-instance from the current mode
// byte. The first sector after ReadN/ReadS is slower than the
// steady-state stream; see `initial_sector_read_cycles` and
// `sector_read_cycles`.

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct XaCoding {
    stereo: bool,
    freq: u32,
    nbits: u8,
}

/// A chained response scheduled when this event fires. Redux enqueues
/// a command's long-running completion from inside the first-response
/// interrupt handler, so the second deadline is relative to the actual
/// first IRQ service cycle rather than the original command write.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingFollowup {
    command: u8,
    delay: u64,
    irq: IrqType,
    bytes: Vec<u8>,
}

/// A deferred response: when `bus.cycles` passes `deadline` (an
/// absolute bus-cycle count), the event's bytes land in the response
/// FIFO and its IRQ type fires.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingEvent {
    command: u8,
    deadline: u64,
    irq: IrqType,
    bytes: Vec<u8>,
    followup: Option<PendingFollowup>,
}

/// One command-port write captured for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CdRomCommandLogEntry {
    /// Bus cycle when the command byte was written.
    pub cycle: u64,
    /// Command byte written to `0x1F801801`.
    pub command: u8,
    /// Parameter FIFO contents drained by this command.
    pub params: [u8; PARAM_FIFO_DEPTH],
    /// Number of valid bytes in [`CdRomCommandLogEntry::params`].
    pub param_len: u8,
}

/// One IRQ response packet delivered by the controller, captured for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CdRomResponseLogEntry {
    /// Bus cycle when the response IRQ packet was latched.
    pub cycle: u64,
    /// IRQ type associated with this packet.
    pub irq: IrqType,
    /// Response FIFO contents published by this packet.
    pub bytes: [u8; RESPONSE_FIFO_DEPTH],
    /// Number of valid bytes in [`CdRomResponseLogEntry::bytes`].
    pub len: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum DriveState {
    Stopped,
    Standby,
    LidOpen,
    RescanCd,
    PrepareCd,
}

/// CD-ROM controller state.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CdRom {
    /// Index register low 2 bits -- selects the register visible at
    /// each sub-port for the next read/write.
    index: u8,
    /// Drive status byte returned by `GetStat` and embedded in the
    /// first byte of most responses. Read by phase-6c command handlers.
    #[allow(dead_code)]
    drive_status: u8,
    /// Parameter FIFO -- software pushes here before invoking a
    /// command; the controller drains it when the command runs.
    params: VecDeque<u8>,
    /// Response FIFO -- command responses arrive here for software
    /// to pop via `0x1F80_1801`.
    responses: VecDeque<u8>,
    /// Command/parameter transmission busy latch (status bit 7).
    /// Redux sets this when a command byte is written, then clears it
    /// when the corresponding interrupt packet is materialized.
    command_busy: bool,
    /// IRQ enable mask -- low 3 bits; an IRQ fires only if its type
    /// bit is enabled. BIOS writes via `0x1F80_1802 idx=1`.
    irq_mask: u8,
    /// Currently-raised IRQ type; cleared by writing 1s to the low
    /// 3 bits of `0x1F80_1803 idx=1`.
    irq_flag: u8,
    /// Scheduled events waiting for their cycle deadlines. Processed
    /// in order by [`CdRom::tick`].
    pending: VecDeque<PendingEvent>,
    /// Internal lid / rescan timer. Redux drives this via a separate
    /// interrupt slot (`PSXINT_CDRLID`) rather than a visible CDROM
    /// IRQ packet.
    lid_deadline: Option<u64>,
    /// `CdlInit` starts the lid/rescan path when its ACK is serviced,
    /// not at command-write time.
    lid_bootstrap_pending: bool,
    drive_state: DriveState,
    /// `true` once the BIOS has sent an `Init` command and the
    /// motor has completed spinning up. Gates commands that need the
    /// motor (Seek, Read, Play).
    motor_on: bool,
    /// Whether a disc is currently inserted. For "no-disc" boots
    /// this stays `false`, and commands that expect a disc (GetID,
    /// ReadN) return error responses.
    disc_present: bool,
    /// Most-recent SetLoc BCD target (minute, second, frame). Used
    /// by SeekL / ReadN to know where to go.
    setloc_msf: (u8, u8, u8),
    /// Whether a new SetLoc target is waiting to be applied to the
    /// live read/play head. Redux doesn't move immediately on SetLoc;
    /// it latches the target and consumes it on Seek/Read/Play.
    setloc_pending: bool,
    /// Controller-firmware GetStat service phase. Every fifth mounted-media
    /// status request crosses the maintenance pass measured on SCPH-9902.
    #[serde(default)]
    getstat_commands: u64,
    /// Total commands dispatched since reset -- diagnostic counter.
    /// Excluded from save states.
    #[serde(skip)]
    commands_dispatched: u64,
    /// Diagnostic histogram of each command byte seen -- `[0x00..=0x1F]`
    /// is enough to capture every BIOS command. Exposed via
    /// [`CdRom::command_histogram`] for `smoke_draw`. Excluded from
    /// save states.
    #[serde(skip)]
    command_hist: [u32; 32],
    /// The most recently dispatched command byte. Diagnostic only
    /// -- used by the `cdrom_probe` example to log exactly which
    /// command was just issued at each `commands_dispatched` bump.
    /// Excluded from save states.
    #[serde(skip)]
    last_command: u8,
    /// Bounded command log. Disabled by default; probes opt in with
    /// [`CdRom::enable_command_log`] when command parameters matter.
    /// Excluded from save states.
    #[serde(skip)]
    command_log: Vec<CdRomCommandLogEntry>,
    /// Max length of [`CdRom::command_log`]. Excluded from save states
    /// (resets to "logging off"; probes re-enable explicitly).
    #[serde(skip)]
    command_log_cap: usize,
    /// Bounded response-packet log for diagnostics. Excluded from
    /// save states.
    #[serde(skip)]
    response_log: Vec<CdRomResponseLogEntry>,
    /// Max length of [`CdRom::response_log`]. Excluded from save states.
    #[serde(skip)]
    response_log_cap: usize,
    /// Total bytes popped from the data FIFO via MMIO reads.
    /// Diagnostic. If this grows in lockstep with DataReady events
    /// the BIOS is actually consuming the sectors we delivered; if
    /// it's stuck, the BIOS's read path is blocked on something
    /// we're not signalling (BFRD / request-register / IRQ ack).
    /// Excluded from save states.
    #[serde(skip)]
    data_fifo_pops: u64,
    /// Bus cycle at which the currently-dispatching command was
    /// received. `queue_command` stashes the caller-supplied `now`
    /// here so `schedule_*_response` helpers can compute absolute
    /// deadlines without threading `now` through every command
    /// handler.
    scheduling_cycle: u64,
    /// Whether the command currently dispatching was issued while CD-DA audio
    /// was already playing. Latched in `queue_command` before any handler
    /// mutates the play state, and consumed by the `schedule_*_response`
    /// helpers to add [`CDDA_BUSY_RESPONSE_CYCLES`] to the acknowledge delay --
    /// modelling the busy single CD controller. See that constant.
    cmd_issued_during_cdda: bool,
    /// Per-IrqType raise histogram -- indexed by `IrqType`
    /// discriminant (0..=5). Probes read this to tell
    /// Acknowledge/DataReady/Complete/Error counts apart when the
    /// aggregate CDROM raise count looks suspicious. Excluded from
    /// save states.
    #[serde(skip)]
    pub irq_type_counts: [u64; 6],
    /// Per-raise log of `(cycle_when_raised, irq_type_discriminant)`
    /// tuples. Populated only when `cdrom_irq_log_cap > 0`, capped
    /// at that length to keep memory bounded in long runs. Probes
    /// compare this sequence against Redux's silent-run CDROM-IRQ
    /// log to pinpoint which specific IRQ fires at a divergent
    /// cycle. Excluded from save states.
    #[serde(skip)]
    pub cdrom_irq_log: Vec<(u64, u8)>,
    /// Max length of `cdrom_irq_log` -- 0 disables logging (the
    /// default, to avoid the per-raise allocation in production
    /// runs). Probes set this via [`CdRom::enable_irq_log`]. Excluded
    /// from save states.
    #[serde(skip)]
    cdrom_irq_log_cap: usize,
    /// Count of `schedule_sector_event_at` calls. Diagnostic only --
    /// the BIOS should see one DataReady per sector read at the
    /// current CD speed's cadence, so this should match the sector
    /// count the game requested. A blown-out number means we're
    /// chaining extra events somewhere. Excluded from save states.
    #[serde(skip)]
    pub sector_events_scheduled: u64,
    /// DataReady sector events that advanced the read stream without
    /// raising a CPU-visible CDROM IRQ. Diagnostic for XA streams.
    /// Excluded from save states.
    #[serde(skip)]
    data_ready_suppressed: u64,
    /// OR of submode bytes across every suppressed sector. Diagnostic:
    /// reveals whether an EOF (0x80) sector was suppressed rather than
    /// raising INT1. Excluded from save states.
    #[serde(skip)]
    dbg_suppressed_submode_or: u8,
    /// Loaded disc image, if any. When `Some`, `disc_present` is also
    /// true and GetID / ReadN follow the disc-present paths; when
    /// `None`, they fall back to the "please insert disc" path.
    ///
    /// Excluded from save states: a full disc image can run past
    /// 700 MB, so embedding it would balloon every save file and pull
    /// the whole game data through postcard on every save/load. The
    /// frontend's load path is responsible for remounting the disc
    /// from the game already known to be running (`AppState::current_game`)
    /// before the restored `CdRom` is used.
    #[serde(skip)]
    disc: Option<Disc>,
    /// Data FIFO -- 2048 bytes of sector user data, drained by MMIO
    /// reads at `0x1F80_1802` or by DMA channel 3. Filled by each
    /// DataReady event during an active ReadN / ReadS.
    data_fifo: VecDeque<u8>,
    /// Sectors that have arrived but which software has not started reading.
    /// The controller holds a small ring of these; a read that falls further
    /// behind than the ring is deep loses the oldest, which is how a guest
    /// that services its CD interrupt too slowly ends up with a hole in its
    /// stream instead of a stall. [`SECTOR_BUFFERS`] counts this plus the one
    /// being drained through `data_fifo`.
    waiting_sectors: VecDeque<(u32, Vec<u8>)>,
    /// Sectors lost that way. Diagnostic only: the guest cannot see it, which
    /// is precisely why it is worth counting.
    dropped_sectors: u64,
    /// Where on the disc the drive was the first and last time it lost one.
    /// A count alone does not say which read fell behind; a range does.
    dropped_lba_first: u32,
    dropped_lba_last: u32,
    /// Redux's DRQSTS/data-ready latch (status bit 6). A fresh sector
    /// sets this even before software has armed a transfer via the
    /// request register; stray reads with no transfer armed clear it
    /// back down without consuming the buffered bytes.
    data_fifo_ready: bool,
    /// Set by request-register bit 7 (`0x1F80_1803` index 0). MMIO
    /// data reads may only drain the current sector while this is
    /// armed; DMA3 reads the transfer buffer once a sector is ready,
    /// matching Redux's `m_read` gate rather than the request latch.
    data_transfer_active: bool,
    /// Last read sector header (MM, SS, FF, mode) -- returned by
    /// `GetlocL` after the drive has actually delivered a sector.
    last_sector_header: [u8; 4],
    /// Last read sector subheader (file, channel, submode, coding) --
    /// likewise returned by `GetlocL`.
    last_sector_subheader: [u8; 4],
    /// Whether `last_sector_header` / `last_sector_subheader`
    /// currently hold real sector data.
    last_sector_header_valid: bool,
    /// Set while a read is in progress; controls whether new
    /// DataReady events chain into further sectors.
    reading: bool,
    /// Redux delays one sector event by `cdReadTime / 2` if the CPU's
    /// CDROM IRQ bit is still pending when the next read interrupt
    /// matures. This latch prevents repeated long delays for the same
    /// sector; it resets after a sector is actually delivered.
    read_rescheduled: bool,
    /// Next sector LBA to deliver during an active read.
    read_lba: u32,
    /// Sample cursor inside the current CD-DA sector.
    cdda_sample_index: usize,
    /// Cycle at which an accepted Play finishes travelling to its track and
    /// audio actually starts. `None` when the drive is not on its way
    /// anywhere. See [`timing::seek_cycles`] for why this exists.
    cdda_seek_done_at: Option<u64>,
    /// Redux tracks whether the drive has already completed a seek and
    /// uses that to pick the short 0x800-cycle SeekL/SeekP follow-up
    /// path on subsequent seeks.
    seek_done: bool,
    /// Redux inserts a long delay before the second sector after a
    /// relocated read starts (`m_locationChanged`). Without it we
    /// stream multiple sectors where the hardware only delivered one.
    location_changed: bool,
    /// Last SetMode byte written by the CPU. Bit layout:
    ///   0: CD-DA enable (for Play command)
    ///   1: auto-pause on track boundary
    ///   2: play-report enable
    ///   3: XA filter enable
    ///   4: ignore-bit (internal)
    ///   5: sector size (0 = 2048 bytes / data only, 1 = 2340 bytes / full)
    ///   6: XA ADPCM enable
    ///   7: speed (0 = single-speed 1x, 1 = double-speed 2x)
    ///
    /// We act on bit 7 (speed) for sector pacing and bit 6 (XA
    /// ADPCM enable) for in-stream audio decode.
    mode: u8,
    /// CD-XA mute latch (`Mute` / `Demute` commands).
    muted: bool,
    /// XA filter state written by `Setfilter` and reported back by
    /// `Getparam`. XA-streaming games use this to confirm the drive
    /// latched their file/channel filter before they start reads.
    xa_filter_file: u8,
    xa_filter_channel: u8,
    /// XA stream state: `1` = next matching audio sector is the
    /// first one in a stream, `0` = continuing stream, `-1` =
    /// decode disabled until the next `Read*`.
    xa_first_sector: i8,
    /// Parsed XA coding for the active stream. Redux parses this on
    /// the first sector, resets decoder history if it changes, then
    /// reuses it for successive sectors instead of trusting every
    /// sector's coding byte.
    xa_coding: Option<XaCoding>,
    /// Live CD-XA volume matrix, applied before samples reach the SPU.
    attenuator_left_to_left: u8,
    attenuator_left_to_right: u8,
    attenuator_right_to_left: u8,
    attenuator_right_to_right: u8,
    /// Shadow volume matrix registers, committed when software writes
    /// bit 5 on `0x1F80_1803` with index 3.
    attenuator_left_to_left_t: u8,
    attenuator_left_to_right_t: u8,
    attenuator_right_to_left_t: u8,
    attenuator_right_to_right_t: u8,
    /// Decoded stereo sample buffer -- filled by XA ADPCM decode
    /// when an audio sector arrives. Drained by the bus each tick
    /// and pushed to the SPU's CD audio input. Unlike the final host
    /// output queue, these samples can still affect emulated SPU RAM,
    /// capture-buffer IRQs, and reverb, so they must round-trip.
    cd_audio: VecDeque<(i16, i16)>,
    /// XA ADPCM decoder left-channel filter history (y0, y1).
    /// Persists across blocks within a file; reset between XA
    /// files / on Pause.
    xa_left: crate::spu::XaDecoderState,
    /// XA right-channel history.
    xa_right: crate::spu::XaDecoderState,
}

impl CdRom {
    /// Fresh controller -- shell open, motor off, all FIFOs empty,
    /// IRQ disabled. Matches hardware state a few cycles after reset,
    /// before the BIOS has had a chance to write anything.
    pub fn new() -> Self {
        Self {
            index: 0,
            // Cold boot: on a closed shell with no disc seated, we
            // want the BIOS to reach the "Please insert disc" shell --
            // that needs SHELL_OPEN clear (lid closed) so the Init
            // command spins the motor up without erroring.
            drive_status: 0,
            params: VecDeque::with_capacity(PARAM_FIFO_DEPTH),
            responses: VecDeque::with_capacity(RESPONSE_FIFO_DEPTH),
            command_busy: false,
            // Redux initializes m_reg2 (the CDROM IRQ mask) to 0x1F on
            // reset (cdrom.cc:1562) so all five IRQ types are enabled
            // out of the gate. We used to start at 0, which blocked every
            // CDROM IRQ from reaching the CPU at boot. That was masked in
            // earlier runs because we also ignored the mask when raising
            // (see `should_wake_cpu`); after adding the mask gate, our 0
            // initial value caused CDROM IRQs to silently latch without
            // waking CPU -- BIOS boot then polled the latched flag instead
            // of getting its usual ISR-driven ack, drifting from Redux.
            irq_mask: 0x1F,
            irq_flag: 0,
            pending: VecDeque::new(),
            lid_deadline: None,
            lid_bootstrap_pending: false,
            drive_state: DriveState::Stopped,
            motor_on: false,
            disc_present: false,
            setloc_msf: (0, 0, 0),
            setloc_pending: false,
            getstat_commands: 0,
            commands_dispatched: 0,
            command_hist: [0; 32],
            last_command: 0,
            command_log: Vec::new(),
            command_log_cap: 0,
            response_log: Vec::new(),
            response_log_cap: 0,
            data_fifo_pops: 0,
            scheduling_cycle: 0,
            cmd_issued_during_cdda: false,
            irq_type_counts: [0; 6],
            cdrom_irq_log: Vec::new(),
            cdrom_irq_log_cap: 0,
            sector_events_scheduled: 0,
            data_ready_suppressed: 0,
            dbg_suppressed_submode_or: 0,
            disc: None,
            data_fifo: VecDeque::new(),
            waiting_sectors: VecDeque::new(),
            dropped_sectors: 0,
            dropped_lba_first: 0,
            dropped_lba_last: 0,
            data_fifo_ready: false,
            data_transfer_active: false,
            last_sector_header: [0; 4],
            last_sector_subheader: [0; 4],
            last_sector_header_valid: false,
            reading: false,
            read_rescheduled: false,
            read_lba: 0,
            cdda_sample_index: 0,
            cdda_seek_done_at: None,
            seek_done: false,
            location_changed: false,
            // Power-on mode: double-speed, no XA, data-only 2048-byte
            // sectors. Matches the BIOS's probe-time expectation -- it
            // issues SetMode 0x80 (double-speed) before its first
            // ReadN. A fresh emulator reset without an intervening
            // SetMode still uses double-speed, matching the prior
            // behaviour of always-CD_READ_TIME pacing.
            mode: 0x80,
            muted: false,
            xa_filter_file: 1,
            xa_filter_channel: 1,
            xa_first_sector: 0,
            xa_coding: None,
            attenuator_left_to_left: 0x80,
            attenuator_left_to_right: 0x00,
            attenuator_right_to_left: 0x00,
            attenuator_right_to_right: 0x80,
            attenuator_left_to_left_t: 0x00,
            attenuator_left_to_right_t: 0x00,
            attenuator_right_to_left_t: 0x00,
            attenuator_right_to_right_t: 0x00,
            cd_audio: VecDeque::new(),
            xa_left: crate::spu::XaDecoderState::new(),
            xa_right: crate::spu::XaDecoderState::new(),
        }
    }

    /// Pull all pending CD audio samples -- called by the bus once
    /// per frame, then forwarded to `Spu::feed_cd_audio`. Returns
    /// stereo pairs in playback order (oldest first). Empty when
    /// no XA / CD-DA decode has produced samples since the last
    /// drain.
    pub fn drain_cd_audio(&mut self) -> Vec<(i16, i16)> {
        self.cd_audio.drain(..).collect()
    }

    /// Decode `sample_count` CD-DA samples from the current Play head.
    ///
    /// The bus calls this with the same count it is about to run
    /// through the SPU, so Red Book audio stays locked to the SPU's
    /// 44.1 kHz output clock without a second timing source.
    pub fn pump_cdda_samples(&mut self, mut sample_count: usize) {
        if self.drive_status & drive_status_bit::PLAYING == 0 {
            return;
        }
        while sample_count != 0 {
            let count = sample_count.min(CDDA_SAMPLES_PER_SECTOR - self.cdda_sample_index);
            let Some(mut samples) = self.decode_cdda_chunk(count) else {
                self.finish_cdda_playback();
                break;
            };
            if !self.muted {
                self.attenuate_cd_samples(&mut samples);
                self.append_cd_audio_samples(&samples);
            }
            self.cdda_sample_index += count;
            if self.cdda_sample_index == CDDA_SAMPLES_PER_SECTOR {
                self.cdda_sample_index = 0;
                self.read_lba = self.read_lba.wrapping_add(1);
            }
            sample_count -= count;
        }
    }

    /// Queue depth of the CD audio buffer -- diagnostic.
    pub fn cd_audio_queue_len(&self) -> usize {
        self.cd_audio.len()
    }

    /// Live SetMode byte -- diagnostic for XA / raw-sector streaming.
    pub fn debug_mode(&self) -> u8 {
        self.mode
    }

    /// Live XA file/channel filter -- diagnostic for STR/XA streams.
    pub fn debug_xa_filter(&self) -> (u8, u8) {
        (self.xa_filter_file, self.xa_filter_channel)
    }

    /// Next LBA the active ReadN/ReadS stream will try to deliver.
    pub fn debug_read_lba(&self) -> u32 {
        self.read_lba
    }

    /// Last sector header/subheader delivered by the drive.
    pub fn debug_last_sector(&self) -> Option<([u8; 4], [u8; 4])> {
        self.last_sector_header_valid
            .then_some((self.last_sector_header, self.last_sector_subheader))
    }

    fn commit_attenuator(&mut self) {
        self.attenuator_left_to_left = self.attenuator_left_to_left_t;
        self.attenuator_left_to_right = self.attenuator_left_to_right_t;
        self.attenuator_right_to_left = self.attenuator_right_to_left_t;
        self.attenuator_right_to_right = self.attenuator_right_to_right_t;
    }

    fn reset_xa_stream(&mut self) {
        self.xa_first_sector = 0;
        self.xa_coding = None;
        self.xa_left.reset();
        self.xa_right.reset();
        self.cd_audio.clear();
    }

    fn finish_cdda_playback(&mut self) {
        self.halt_cdda();
        self.cdda_sample_index = 0;
    }

    /// Take the drive off CD-DA: both the playback and any seek still on its
    /// way to a track. Anything that ends audio has to cancel the seek too,
    /// or a Play abandoned mid-journey would still arrive and start playing.
    fn halt_cdda(&mut self) {
        self.drive_status &= !(drive_status_bit::PLAYING | drive_status_bit::SEEKING);
        self.cdda_seek_done_at = None;
    }

    fn suppress_data_ready(&mut self) -> bool {
        self.data_ready_suppressed = self.data_ready_suppressed.saturating_add(1);
        false
    }

    fn clear_data_fifo(&mut self) {
        self.data_fifo.clear();
        self.waiting_sectors.clear();
        self.data_fifo_ready = false;
        self.data_transfer_active = false;
    }

    /// Hand a freshly-read sector to software.
    ///
    /// The drive does not wait: sectors keep arriving at the read rate whether
    /// or not the last one was collected. If software is still draining a
    /// sector the new one queues behind it, and once the ring is full the
    /// oldest is overwritten and simply lost. That silent loss is the whole
    /// point of modelling this: a guest whose interrupt handler runs late
    /// reads a stream with a hole in it, and nothing tells it so.
    fn push_sector(&mut self, lba: u32, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if self.data_fifo.is_empty() && self.waiting_sectors.is_empty() {
            self.data_fifo.extend(bytes);
            self.data_fifo_ready = true;
            return;
        }
        // One is in `data_fifo`, so the queue holds the rest of the ring.
        while self.waiting_sectors.len() >= SECTOR_BUFFERS - 1 {
            let (lost, _) = self
                .waiting_sectors
                .pop_front()
                .expect("loop condition guarantees an element");
            if self.dropped_sectors == 0 {
                self.dropped_lba_first = lost;
                if std::env::var_os("PSOXIDE_TRACE_SECTOR_DROP").is_some() {
                    eprintln!(
                        "[drop] lba={lost} read_lba={} waiting={} reading={} mode=0x{:02X} \
                         irq_flag={} fifo={} last_cmd=0x{:02X} scheduling_cycle={}",
                        self.read_lba,
                        self.waiting_sectors.len(),
                        self.reading,
                        self.mode,
                        self.irq_flag,
                        self.data_fifo.len(),
                        self.last_command,
                        self.scheduling_cycle,
                    );
                }
            }
            self.dropped_lba_last = lost;
            self.dropped_sectors = self.dropped_sectors.saturating_add(1);
        }
        self.waiting_sectors.push_back((lba, bytes));
    }

    fn pop_data_fifo_byte(&mut self) -> u8 {
        if !self.data_transfer_active {
            self.data_fifo_ready = false;
            return 0;
        }

        let byte = self.data_fifo.pop_front().unwrap_or(0);
        if self.data_fifo.is_empty() {
            self.data_transfer_active = false;
            self.advance_to_next_sector();
        }
        byte
    }

    /// Promote whatever arrived while software was busy with the last sector.
    ///
    /// Both drains have to do this. Software reads a sector either through the
    /// data port or, far more often, by pointing DMA3 at it, and if only one
    /// of those advances the queue then the other stops collecting after its
    /// first sector and everything behind it is dropped as overflow.
    fn advance_to_next_sector(&mut self) {
        match self.waiting_sectors.pop_front() {
            Some((_, next)) => {
                self.data_fifo.extend(next);
                self.data_fifo_ready = true;
            }
            None => self.data_fifo_ready = false,
        }
    }

    /// Point software at the newest sector as its interrupt is delivered, and
    /// give up on everything that arrived behind it.
    ///
    /// This is the part that punishes a late handler. The controller does not
    /// hand over a backlog to work through: when the interrupt finally reaches
    /// the CPU, the read position is taken from wherever the decoder has got
    /// to, so every sector that landed while the previous interrupt sat
    /// unserviced is stepped over and never seen. Losing data therefore starts
    /// as soon as software is one sector late, not once the ring is full.
    fn snap_to_newest_sector(&mut self) {
        let Some((newest_lba, newest)) = self.waiting_sectors.pop_back() else {
            return;
        };
        while let Some((skipped, _)) = self.waiting_sectors.pop_front() {
            if self.dropped_sectors == 0 {
                self.dropped_lba_first = skipped;
                if std::env::var_os("PSOXIDE_TRACE_SECTOR_DROP").is_some() {
                    eprintln!(
                        "[drop/snap] lba={skipped} newest={} backlog={} reading={} fifo={}",
                        newest_lba,
                        self.waiting_sectors.len() + 1,
                        self.reading,
                        self.data_fifo.len(),
                    );
                }
            }
            self.dropped_lba_last = skipped;
            self.dropped_sectors = self.dropped_sectors.saturating_add(1);
        }
        // The one software had not started on yet is stepped over too.
        if !self.data_fifo.is_empty() {
            self.data_fifo.clear();
        }
        self.data_fifo.extend(newest);
        self.data_fifo_ready = true;
    }

    fn append_cd_audio_samples(&mut self, samples: &[(i16, i16)]) {
        let cap = 44_100; // ~1 s at SPU rate
        let overflow = (self.cd_audio.len() + samples.len()).saturating_sub(cap);
        for _ in 0..overflow {
            self.cd_audio.pop_front();
        }
        self.cd_audio.extend(samples.iter().copied());
    }

    fn decode_cdda_chunk(&self, count: usize) -> Option<Vec<(i16, i16)>> {
        let raw = self.disc.as_ref()?.read_cdda_sector(self.read_lba)?;
        let start = self.cdda_sample_index * CDDA_BYTES_PER_SAMPLE;
        let end = start + count * CDDA_BYTES_PER_SAMPLE;
        let bytes = raw.get(start..end)?;
        let mut out = Vec::with_capacity(count);
        for frame in bytes.chunks_exact(CDDA_BYTES_PER_SAMPLE) {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            let right = i16::from_le_bytes([frame[2], frame[3]]);
            out.push((left, right));
        }
        Some(out)
    }

    fn attenuate_cd_samples(&self, samples: &mut [(i16, i16)]) {
        let ll = self.attenuator_left_to_left as i32;
        let lr = self.attenuator_left_to_right as i32;
        let rl = self.attenuator_right_to_left as i32;
        let rr = self.attenuator_right_to_right as i32;

        if lr == 0 && rl == 0 && (0x78..=0x88).contains(&ll) && (0x78..=0x88).contains(&rr) {
            return;
        }

        for (l, r) in samples.iter_mut() {
            let mixed_l = ((*l as i32) * ll + (*r as i32) * rl) >> 7;
            let mixed_r = ((*r as i32) * rr + mixed_l * lr) >> 7;
            *l = mixed_l.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            *r = mixed_r.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
    }

    /// Load a disc image. After this, GetID returns the licensed-disc
    /// response and ReadN streams real sector data through the
    /// DataReady event chain.
    ///
    /// Inserting a disc also spins up the motor. On real hardware the
    /// motor starts when the shell is closed with a disc seated, even
    /// before the BIOS issues `Init`. Without this, `GetID`'s stat
    /// byte reports motor-off (0x00), which the BIOS's shell poll
    /// treats as "drive not ready yet" and never advances past the
    /// disc-probe loop to issue `SetLoc + SeekL + ReadN`.
    ///
    /// `insert_disc(None)` "ejects" -- disc_present flips false, the
    /// motor stops, any in-flight read is cancelled, and the next
    /// `GetID` returns the no-disc response again.
    pub fn insert_disc(&mut self, disc: Option<Disc>) {
        self.disc = disc;
        self.disc_present = self.disc.is_some();
        self.motor_on = self.disc_present;
        self.drive_state = if self.disc_present {
            DriveState::Standby
        } else {
            DriveState::Stopped
        };
        self.lid_deadline = None;
        self.lid_bootstrap_pending = false;
        self.reading = false;
        self.read_rescheduled = false;
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.clear_data_fifo();
        self.cd_audio.clear();
        self.last_sector_header = [0; 4];
        self.last_sector_subheader = [0; 4];
        self.last_sector_header_valid = false;
        self.xa_first_sector = 0;
        self.xa_left.reset();
        self.xa_right.reset();
    }

    /// Splice a disc handle into `self.disc` without touching any
    /// other register/FIFO/motor state. Save-state loads need this
    /// instead of [`CdRom::insert_disc`]: the restored snapshot
    /// already carries the correct motor/drive/read state for
    /// wherever the game was mid-transfer, and `insert_disc`'s
    /// lid-open/close-style reset (motor spin-up, FIFO clear, drive
    /// state reseat, …) would stomp exactly the state we just
    /// restored. This exists solely to reattach the disc bytes that
    /// [`CdRom::disc`] deliberately excludes from serialization.
    pub fn restore_disc_for_savestate(&mut self, disc: Option<Disc>) {
        self.disc = disc;
    }

    /// Move the mounted disc out, leaving `None` behind. Used to hand
    /// a disc off to a save-state-restored `CdRom` (see
    /// [`CdRom::restore_disc_for_savestate`]) without cloning the
    /// image, which can run past 700 MB for a full CD dump.
    pub fn take_disc(&mut self) -> Option<Disc> {
        self.disc.take()
    }

    /// `true` when `phys` is inside the CD-ROM MMIO range.
    pub fn contains(phys: u32) -> bool {
        (BASE..END).contains(&phys)
    }

    /// 8-bit read through the index-selected register at `phys`.
    pub fn read8(&mut self, phys: u32) -> u8 {
        let offset = (phys - BASE) as u8;
        match (offset, self.index) {
            // 0x1F80_1800 -- status byte (same at every index).
            (0, _) => self.status_byte(),
            // 0x1F80_1801 -- response FIFO (any index).
            (1, _) => self.pop_response(),
            // 0x1F80_1802 -- data FIFO (any index).
            (2, _) => {
                self.data_fifo_pops = self.data_fifo_pops.saturating_add(1);
                self.pop_data_fifo_byte()
            }
            // 0x1F80_1803 -- index-dependent:
            //   idx=0 → interrupt enable,
            //   idx=1 → interrupt flag,
            //   idx=2 → mirror of enable,
            //   idx=3 → mirror of flag.
            (3, 0) | (3, 2) => self.irq_mask | 0xE0,
            (3, 1) | (3, 3) => self.irq_flag | 0xE0,
            _ => 0,
        }
    }

    /// 8-bit write through the index-selected register at `phys`.
    pub fn write8(&mut self, phys: u32, value: u8) {
        // Back-compat shim: tests that aren't cycle-aware call this
        // variant. For parity-correct scheduling, the bus uses
        // `write8_at` instead so command-port writes carry the
        // current bus cycle through to the CDROM scheduler.
        let _ = self.write8_at(phys, value, 0);
    }

    /// Like [`write8`], but threads the bus cycle through so
    /// `queue_command` can schedule first/second responses with
    /// absolute deadlines anchored on issue time. Matches Redux's
    /// `AddIrqQueue(cmd, delay)` which anchors on `m_regs.cycle` at
    /// the cmd-port write. The previous "relative then rebase on
    /// next tick" scheme lost the BIAS + memory-access cycles of
    /// the SB that issued the command -- surfaced as a 5-cycle late
    /// IRQ dispatch at parity step 89,198,894.
    pub fn write8_at(&mut self, phys: u32, value: u8, now: u64) -> bool {
        let offset = (phys - BASE) as u8;
        match (offset, self.index) {
            // 0x1F80_1800 write -- set the index.
            (0, _) => self.index = value & status_bit::INDEX_MASK,
            // 0x1F80_1801 idx=0 -- command register. Queue for 6b.
            (1, 0) => self.queue_command(value, now),
            // 0x1F80_1801 idx=1/2/3 -- audio sound-map / CD-to-SPU
            // volume.
            (1, 1 | 2) => {}
            (1, 3) => self.attenuator_right_to_right_t = value,
            // 0x1F80_1802 idx=0 -- parameter FIFO push.
            (2, 0) => self.push_param(value),
            // 0x1F80_1802 idx=1 -- interrupt enable.
            (2, 1) => {
                self.irq_mask = value & 0x1F;
                return self.should_wake_cpu();
            }
            // 0x1F80_1802 idx=2/3 -- audio volume.
            (2, 2) => self.attenuator_left_to_left_t = value,
            (2, 3) => self.attenuator_right_to_left_t = value,
            // 0x1F80_1803 idx=0 -- request register (data transfer on,
            // command-buffer reset, etc.). Bit 7 = want-data. Full
            // modelling arrives with sector reads.
            (3, 0) => {
                // Bit 6 = BFRD (reset). If set, clear parameter FIFO.
                if value & 0x40 != 0 {
                    self.params.clear();
                }
                // Bit 7 arms the sector-transfer buffer. Redux gates
                // both MMIO reads and DMA behind this request latch
                // instead of exposing any queued sector bytes
                // immediately when DataReady fires.
                if value & 0x80 != 0 && !self.data_transfer_active {
                    self.data_transfer_active = true;
                }
            }
            // 0x1F80_1803 idx=1 -- acknowledge interrupts (write-1-to-
            // clear on the low 5 bits; bit 6 resets the param FIFO too).
            (3, 1) => {
                self.irq_flag &= !(value & 0x1F);
                if value & 0x40 != 0 {
                    self.params.clear();
                }
            }
            // 0x1F80_1803 idx=2/3 -- audio volume matrix.
            (3, 2) => self.attenuator_left_to_right_t = value,
            (3, 3) if value & 0x20 != 0 => {
                self.commit_attenuator();
            }
            _ => {}
        }
        false
    }

    /// Compose the MMIO status byte from live FIFO + index state.
    fn status_byte(&self) -> u8 {
        let mut s = self.index;
        if self.params.is_empty() {
            s |= status_bit::PARAM_FIFO_EMPTY;
        }
        if self.params.len() < PARAM_FIFO_DEPTH {
            s |= status_bit::PARAM_FIFO_NOT_FULL;
        }
        if !self.responses.is_empty() {
            s |= status_bit::RESPONSE_FIFO_NOT_EMPTY;
        }
        if self.data_fifo_ready {
            s |= status_bit::DATA_FIFO_NOT_EMPTY;
        }
        if self.command_busy {
            s |= status_bit::TRANSMISSION_BUSY;
        }
        // ADPCM busy (bit 2) comes from a subsystem we don't expose
        // yet; keep it clear for now.
        s
    }

    fn pop_response(&mut self) -> u8 {
        self.responses.pop_front().unwrap_or(0)
    }

    fn push_param(&mut self, value: u8) {
        if self.params.len() < PARAM_FIFO_DEPTH {
            self.params.push_back(value);
        }
    }

    /// Execute a command received on the command port. The command's
    /// handler synthesises its first (and optional second) response
    /// and schedules them into the pending-events queue.
    ///
    /// A handful of commands use the parameter FIFO for arguments
    /// (SetLoc MSF, SetMode, Test sub-op). The parameters are drained
    /// inline by the handler.
    /// `PSOXIDE_WEDGE_CD=1`: the drive accepts every command and never
    /// answers. Companion to the DMA and GPU injectors, for the same
    /// reason: the emulator's drive always responds, so guest code whose
    /// deadlines or retry logic are wrong still passes headless and only
    /// fails on a console, which costs a burned disc to discover.
    fn cd_wedged() -> bool {
        static WEDGED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *WEDGED.get_or_init(|| std::env::var_os("PSOXIDE_WEDGE_CD").is_some())
    }

    fn queue_command(&mut self, command: u8, now: u64) {
        self.commands_dispatched += 1;
        self.last_command = command;
        if (command as usize) < self.command_hist.len() {
            self.command_hist[command as usize] += 1;
        }
        if Self::cd_wedged() {
            // Accepted, never acknowledged: no response bytes, no IRQ, and
            // the controller stays busy. Every guest wait must reach its own
            // deadline and move on.
            self.responses.clear();
            self.command_busy = true;
            return;
        }
        // Real hardware exposes only one live response packet. A new
        // command drops any unread bytes from the prior packet instead
        // of appending another logical response stream behind them.
        self.responses.clear();
        self.command_busy = true;
        // Stash the issue-time cycle so each `schedule_*_response`
        // call below resolves its absolute deadline against the
        // right anchor, without threading `now` through every
        // command handler's signature.
        self.scheduling_cycle = now;
        // Latch whether CD-DA was already playing *before* this command runs,
        // so the ack-delay helpers can charge the busy-controller penalty even
        // for commands (Play restart, Pause) that change the play state.
        self.cmd_issued_during_cdda = self.drive_status & drive_status_bit::PLAYING != 0;

        // Drain parameters into a local vec -- handlers need them and
        // pop-order matches push-order.
        let params: Vec<u8> = self.params.drain(..).collect();
        if self.command_log.len() < self.command_log_cap {
            let mut logged_params = [0u8; PARAM_FIFO_DEPTH];
            let n = params.len().min(PARAM_FIFO_DEPTH);
            logged_params[..n].copy_from_slice(&params[..n]);
            self.command_log.push(CdRomCommandLogEntry {
                cycle: now,
                command,
                params: logged_params,
                param_len: n as u8,
            });
        }

        match command {
            // Sync / NOP -- acts as GetStat.
            0x00 | 0x01 => self.cmd_getstat(),
            // SetLoc: 3 BCD bytes (minute, second, frame).
            0x02 => self.cmd_setloc(&params),
            // Play (CD-DA): 1 optional byte (track). Starts Red-Book
            // audio playback. We accept the command and drive the
            // reading state machine so the BIOS's follow-up polls see
            // the drive active; actual CD-DA sample data isn't
            // sourced yet (requires track-level data from the .cue).
            0x03 => self.cmd_play(&params),
            // ReadN (read with auto-retry). Without a disc, error.
            0x06 => self.cmd_read(),
            // MotorOn -- some retail loaders wake the spindle
            // explicitly before querying/reading.
            0x07 => self.cmd_motor_on(),
            // Stop: halt motor.
            0x08 => self.cmd_stop(),
            // Pause: halt reads but keep motor on.
            0x09 => self.cmd_pause(),
            // Reset: abort in-flight drive activity, spin the motor,
            // and complete after Redux's long reset delay.
            0x0A => self.cmd_reset(),
            // Mute / Demute.
            0x0B => self.cmd_mute(true),
            0x0C => self.cmd_mute(false),
            // Setfilter -- XA file/channel filter.
            0x0D => self.cmd_set_filter(&params),
            // Getparam -- mode/filter query.
            0x0F => self.cmd_get_param(),
            // GetlocL -- current logical MSF + mode + subheader.
            0x10 => self.cmd_get_loc_l(),
            // GetlocP -- current play position (track, index, MSF).
            0x11 => self.cmd_get_loc_p(),
            // SetSession -- current loader only models session 1, but
            // retail software still expects the completion IRQ.
            0x12 => self.cmd_set_session(&params),
            // GetTN -- first/last track numbers.
            0x13 => self.cmd_get_tn(),
            // GetTD -- start time of a track, or lead-out for track 0.
            0x14 => self.cmd_get_td(&params),
            // ReadS -- read without auto-retry. Data arrives the same
            // way as ReadN for our purposes; games use ReadS for
            // audio/video streaming where a retry would cause hitching.
            0x1B => self.cmd_read(),
            // SetMode: 1 byte (speed, CD-DA enable, filter, etc.).
            // We accept and store; full behaviour in 6d.
            0x0E => self.cmd_setmode(&params),
            // SeekL: seek to last-SetLoc position (logical sectors).
            0x15 | 0x16 => self.cmd_seek(),
            // Test: sub-op in param[0]. Most common: 0x20 = get
            // drive version / BIOS date (6-byte response).
            0x19 => self.cmd_test(&params),
            // GetID: "what kind of disc is this?"
            0x1A => self.cmd_getid(),
            // ReadTOC: re-read the table of contents. The BIOS
            // issues this during disc-boot to learn the track
            // layout; without a response the BIOS hangs waiting
            // for INT2 (Complete), stranding a commercial title / Crash at the
            // Sony splash.
            0x1E => self.cmd_read_toc(),
            // Init -- lid/rescan state machine; no CPU-visible INT2.
            0x1C => self.cmd_init(),
            _ => self.cmd_getstat(),
        }
    }

    // --- Command handlers ---

    /// Absolute deadline for a command's first (acknowledge) response.
    /// The controller sub-CPU responds from its firmware loop. A mounted disc
    /// makes that loop materially busier, so media and no-media commands have
    /// distinct hardware-calibrated acknowledgement floors. CD-DA playback
    /// adds the existing streaming-controller penalty on top.
    fn first_response_deadline(&self) -> u64 {
        let mut delay = if self.disc_present {
            FIRST_RESPONSE_WITH_MEDIA_CYCLES
        } else {
            FIRST_RESPONSE_CYCLES
        };
        if self.cmd_issued_during_cdda {
            delay = delay.saturating_add(CDDA_BUSY_RESPONSE_CYCLES);
        }
        self.scheduling_cycle.saturating_add(delay)
    }

    /// Schedule a first-response IRQ at [`Self::first_response_deadline`].
    fn schedule_first_response(&mut self, bytes: Vec<u8>) {
        self.schedule_first_response_after(bytes, 0);
    }

    fn schedule_first_response_after(&mut self, bytes: Vec<u8>, extra_delay: u64) {
        self.insert_pending_event(PendingEvent {
            command: self.last_command,
            deadline: self.first_response_deadline().saturating_add(extra_delay),
            irq: IrqType::Acknowledge,
            bytes,
            followup: None,
        });
    }

    fn schedule_first_complete_response(&mut self, bytes: Vec<u8>) {
        self.insert_pending_event(PendingEvent {
            command: self.last_command,
            deadline: self.first_response_deadline(),
            irq: IrqType::Complete,
            bytes,
            followup: None,
        });
    }

    /// Schedule a second-response IRQ. `additional_delay` is time
    /// *after* the first response interrupt actually fires. Matches
    /// Redux's `AddIrqQueue(cmd + 0x100, delay)` path inside the
    /// first-response handler.
    fn schedule_second_response(&mut self, bytes: Vec<u8>, additional_delay: u64) {
        self.chain_followup(IrqType::Complete, bytes, additional_delay);
    }

    /// Like [`schedule_second_response`] but the IRQ type is Error
    /// (INT5). Used for the second reply of commands that fail because
    /// the disc isn't present.
    fn schedule_second_error(&mut self, bytes: Vec<u8>, additional_delay: u64) {
        self.chain_followup(IrqType::Error, bytes, additional_delay);
    }

    fn schedule_error_response(&mut self, bytes: Vec<u8>) {
        self.insert_pending_event(PendingEvent {
            command: self.last_command,
            deadline: self.first_response_deadline(),
            irq: IrqType::Error,
            bytes,
            followup: None,
        });
    }

    fn chain_followup(&mut self, irq: IrqType, bytes: Vec<u8>, delay: u64) {
        if let Some(idx) = self
            .pending
            .iter()
            .rposition(|ev| ev.irq == IrqType::Acknowledge && ev.followup.is_none())
        {
            self.pending[idx].followup = Some(PendingFollowup {
                command: self.last_command,
                delay,
                irq,
                bytes,
            });
            return;
        }
        // Fallback for probes/tests that inject events directly.
        self.insert_pending_event(PendingEvent {
            command: self.last_command,
            deadline: self.scheduling_cycle.saturating_add(delay),
            irq,
            bytes,
            followup: None,
        });
    }

    fn insert_pending_event(&mut self, event: PendingEvent) {
        let idx = self
            .pending
            .iter()
            .position(|existing| existing.deadline > event.deadline)
            .unwrap_or(self.pending.len());
        self.pending.insert(idx, event);
    }

    fn drop_pending_command_irq(&mut self, command: u8, irq: IrqType) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|event| !(event.command == command && event.irq == irq));
        let mut dropped = self.pending.len() != before;
        for event in self.pending.iter_mut() {
            if event
                .followup
                .as_ref()
                .is_some_and(|followup| followup.command == command && followup.irq == irq)
            {
                event.followup = None;
                dropped = true;
            }
        }
        dropped
    }

    fn schedule_lid_transition_at(&mut self, now: u64, delay: u64) {
        self.lid_deadline = Some(now.saturating_add(delay));
    }

    fn tick_lid_state_machine(&mut self, cycles_now: u64) {
        while let Some(deadline) = self.lid_deadline {
            if deadline >= cycles_now {
                break;
            }
            self.lid_deadline = None;
            match self.drive_state {
                DriveState::Stopped => {}
                DriveState::Standby => {
                    self.drive_status &= !drive_status_bit::SEEKING;
                    if !self.disc_present {
                        self.drive_status |= drive_status_bit::SHELL_OPEN;
                        self.drive_state = DriveState::LidOpen;
                    }
                }
                DriveState::LidOpen => {
                    if self.disc_present {
                        // SHELL_OPEN stays sticky until a subsequent
                        // GetStat consumes it.
                        self.drive_state = DriveState::RescanCd;
                        self.schedule_lid_transition_at(cycles_now, CD_READ_TIME * 105);
                    } else {
                        self.schedule_lid_transition_at(cycles_now, CD_READ_TIME * 3);
                    }
                }
                DriveState::RescanCd => {
                    self.motor_on = true;
                    self.drive_state = DriveState::PrepareCd;
                    self.schedule_lid_transition_at(cycles_now, LID_PREPARE_SPINUP_CYCLES);
                }
                DriveState::PrepareCd => {
                    self.drive_status |= drive_status_bit::SEEKING;
                    self.drive_state = DriveState::Standby;
                    self.schedule_lid_transition_at(cycles_now, LID_PREPARE_SEEK_CYCLES);
                }
            }
        }
    }

    /// Stop the live sector stream and strip any queued DataReady work
    /// from both the pending queue and ACK followups. Redux cancels the
    /// read interrupt source on ReadN/Pause/Seek/Init/Reset; without
    /// this, stale sectors from an older stream can leak into the next
    /// command sequence.
    fn cancel_pending_data_ready_events(&mut self) {
        self.pending.retain(|ev| ev.irq != IrqType::DataReady);
        for ev in self.pending.iter_mut() {
            if ev
                .followup
                .as_ref()
                .is_some_and(|f| f.irq == IrqType::DataReady)
            {
                ev.followup = None;
            }
        }
        self.read_rescheduled = false;
    }

    fn stat_byte(&self) -> u8 {
        let mut s = self.drive_status & !(drive_status_bit::MOTOR_ON | drive_status_bit::READING);
        if self.motor_on {
            s |= drive_status_bit::MOTOR_ON;
        }
        if self.reading {
            s |= drive_status_bit::READING;
        }
        s
    }

    fn cmd_getstat(&mut self) {
        let stat = self.stat_byte();
        self.getstat_commands = self.getstat_commands.wrapping_add(1);
        let maintenance_delay = if self.disc_present && self.getstat_commands.is_multiple_of(5) {
            GETSTAT_MAINTENANCE_CYCLES
        } else {
            0
        };
        self.schedule_first_response_after(vec![stat], maintenance_delay);
        // Redux keeps STATUS_SHELLOPEN sticky until GetStat observes
        // it, then clears the latched bit after producing the reply
        // unless the lid is genuinely still open.
        if self.drive_state != DriveState::LidOpen {
            self.drive_status &= !drive_status_bit::SHELL_OPEN;
        }
    }

    fn cmd_setloc(&mut self, params: &[u8]) {
        if params.len() >= 3 {
            let next_msf = (params[0], params[1], params[2]);
            let next_lba = msf_to_lba(next_msf.0, next_msf.1, next_msf.2);
            let current_lba = if self.read_lba != 0 {
                self.read_lba
            } else {
                msf_to_lba(self.setloc_msf.0, self.setloc_msf.1, self.setloc_msf.2)
            };
            if next_lba.abs_diff(current_lba) > 16 {
                self.seek_done = false;
            }
            self.setloc_msf = next_msf;
            self.setloc_pending = true;
        }
        let stat = self.stat_byte();
        self.schedule_first_response(vec![stat]);
    }

    fn cmd_setmode(&mut self, params: &[u8]) {
        if let Some(&m) = params.first() {
            if self.mode & 0x40 == 0 && m & 0x40 != 0 {
                self.xa_left.reset();
                self.xa_right.reset();
            }
            self.mode = m;
        }
        let stat = self.stat_byte();
        self.schedule_first_response(vec![stat]);
    }

    fn cmd_mute(&mut self, muted: bool) {
        self.muted = muted;
        self.schedule_first_response(vec![self.stat_byte()]);
    }

    fn cmd_set_filter(&mut self, params: &[u8]) {
        self.xa_filter_file = params.first().copied().unwrap_or(0);
        self.xa_filter_channel = params.get(1).copied().unwrap_or(0);
        self.schedule_first_response(vec![self.stat_byte()]);
    }

    fn cmd_get_param(&mut self) {
        self.schedule_first_response(vec![
            self.stat_byte(),
            self.mode,
            0,
            self.xa_filter_file,
            self.xa_filter_channel,
        ]);
    }

    /// Delay from a ReadN/ReadS command to the first DataReady event.
    /// Redux uses one full CD frame at double-speed here; the console
    /// (records 0x94/0x95) shows one more frame period of rotational
    /// alignment before the first sector at both speeds, so charge 1.5
    /// frames double / 3 frames single ahead of the chained stream.
    fn initial_sector_read_cycles(&self) -> u64 {
        if self.mode & 0x80 != 0 {
            CD_READ_TIME * 3 / 2
        } else {
            CD_READ_TIME * 3
        }
    }

    /// Cycles between chained DataReady events once a sector stream is
    /// active. Redux's `readInterrupt()` schedules steady double-speed
    /// reads at `cdReadTime / 2`, not `cdReadTime`; the old value fed
    /// XA audio at half rate, which made long music streams underrun.
    fn sector_read_cycles(&self) -> u64 {
        if self.mode & 0x80 != 0 {
            CD_READ_TIME / 2
        } else {
            CD_READ_TIME
        }
    }

    fn cmd_stop(&mut self) {
        self.motor_on = false;
        self.drive_state = DriveState::Stopped;
        self.lid_deadline = None;
        self.lid_bootstrap_pending = false;
        self.reading = false;
        self.cancel_pending_data_ready_events();
        self.location_changed = false;
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.reset_xa_stream();
        self.schedule_first_response(vec![self.stat_byte()]);
        let stat = self.stat_byte();
        self.schedule_second_response(vec![stat], SEEK_SECOND_RESPONSE_CYCLES);
    }

    fn cmd_pause(&mut self) {
        let was_motor_on = self.motor_on;
        // Pause halts the sector-read chain but leaves the motor on.
        // Missing this flip meant DataReady events kept chaining
        // `load_next_sector + schedule_sector_event` indefinitely
        // after the BIOS asked us to pause, producing a runaway
        // pending queue that burned the entire CPU budget on
        // peripheral-scheduling overhead.
        self.reading = false;
        self.cancel_pending_data_ready_events();
        self.location_changed = false;
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.reset_xa_stream();
        let ack_stat = self.stat_byte();
        self.schedule_first_response(vec![ack_stat]);
        let stat = self.stat_byte();
        // Redux uses a short ~7000-cycle follow-up when the drive is
        // already spun up ("standby"), and a much longer completion
        // only when pausing from a stopped / not-ready state. a commercial title hits
        // the standby path: without the short follow-up, Redux raises a
        // general CDROM IRQ ~7k cycles later and we don't.
        let delay = if was_motor_on {
            PAUSE_COMPLETE_CYCLES_STANDBY
        } else if self.mode & 0x80 != 0 {
            PAUSE_COMPLETE_CYCLES_ACTIVE * 2
        } else {
            PAUSE_COMPLETE_CYCLES_ACTIVE
        };
        self.schedule_second_response(vec![stat], delay);
    }

    fn cmd_motor_on(&mut self) {
        self.motor_on = true;
        self.drive_state = DriveState::Standby;
        self.schedule_first_response(vec![self.stat_byte()]);
    }

    /// CdlReadToc (0x1E): re-scan the disc table-of-contents.
    /// Two-part response:
    /// - INT3 (Acknowledge) with stat, immediately.
    /// - INT2 (Complete) with stat, ~20 M cycles later (Redux:
    ///   `cdReadTime * 180 / 4 = 20_321_280`). No track data is
    ///   returned in either response -- the BIOS queries individual
    ///   track info via GetTD after ReadTOC completes.
    ///
    /// The BIOS's disc-boot sequence blocks on the INT2 here; we
    /// used to fall through to `cmd_getstat` on 0x1E, which only
    /// emitted the INT3 and left the BIOS waiting forever on the
    /// INT2. Surfaced as a commercial title + Crash hanging on the Sony splash
    /// at step ~90 M.
    fn cmd_read_toc(&mut self) {
        let stat = self.stat_byte();
        self.schedule_first_response(vec![stat]);
        // Redux value: `cdReadTime * 180 / 4`. We inline the
        // literal to avoid introducing a new named constant
        // here.
        const READ_TOC_SECOND_RESPONSE_CYCLES: u64 = 451_584 * 180 / 4;
        self.schedule_second_response(vec![stat], READ_TOC_SECOND_RESPONSE_CYCLES);
    }

    fn cmd_init(&mut self) {
        // Init is a drive reset -- also halt any in-flight read so
        // DataReady chains from a previous ReadN don't keep firing
        // across the reset boundary.
        self.reading = false;
        self.cancel_pending_data_ready_events();
        self.location_changed = false;
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.clear_data_fifo();
        self.reset_xa_stream();
        self.muted = false;
        self.last_sector_header = [0; 4];
        self.last_sector_subheader = [0; 4];
        self.last_sector_header_valid = false;
        // Redux returns only the pre-init ACK here; the later 20480
        // cycle work happens on the lid/rescan state machine, not as a
        // second CPU-visible CDROM completion IRQ.
        self.schedule_first_response(vec![self.stat_byte()]);
        self.seek_done = true;
        self.motor_on = true;
        self.drive_status |= drive_status_bit::SHELL_OPEN;
        self.drive_state = DriveState::RescanCd;
        self.lid_deadline = None;
        self.lid_bootstrap_pending = true;
    }

    fn cmd_set_session(&mut self, _params: &[u8]) {
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        self.schedule_first_response(vec![self.stat_byte()]);
        self.schedule_second_response(vec![self.stat_byte()], 33_868);
    }

    fn cmd_reset(&mut self) {
        let ack_pending = self.drop_pending_command_irq(0x0A, IrqType::Acknowledge);
        let complete_pending = self.drop_pending_command_irq(0x0A, IrqType::Complete);
        self.responses.clear();
        self.reading = false;
        self.cancel_pending_data_ready_events();
        self.seek_done = true;
        self.setloc_pending = false;
        self.location_changed = false;
        self.drive_status &= !(drive_status_bit::PLAYING
            | drive_status_bit::READING
            | drive_status_bit::SEEKING
            | drive_status_bit::ERROR
            | drive_status_bit::SEEK_ERROR);
        self.cdda_seek_done_at = None;
        self.cdda_sample_index = 0;
        self.clear_data_fifo();
        self.reset_xa_stream();
        self.last_sector_header = [0; 4];
        self.last_sector_subheader = [0; 4];
        self.last_sector_header_valid = false;
        self.mode = 0x20;
        self.motor_on = true;
        self.drive_state = DriveState::Standby;
        self.lid_deadline = None;
        self.lid_bootstrap_pending = false;
        self.drive_status |= drive_status_bit::MOTOR_ON;
        self.muted = false;
        let stat = self.stat_byte();
        if complete_pending && !ack_pending {
            // Redux's `m_irqRepeated` path leaves `m_irq` pointing at
            // CdlReset+0x100. The replacement 0x800-cycle interrupt
            // therefore publishes the pending completion, not another
            // ACK. A commercial BIOS reset loop depends on seeing that INT2.
            self.schedule_first_complete_response(vec![stat]);
        } else {
            self.schedule_first_response(vec![stat]);
            self.schedule_second_response(vec![stat], RESET_SECOND_RESPONSE_CYCLES);
        }
    }

    fn cmd_seek(&mut self) {
        // Need a disc / motor. Without disc we still "seek" but it
        // succeeds immediately on the real drive -- BIOS rarely calls
        // SeekL without a disc.
        self.reading = false;
        self.cancel_pending_data_ready_events();
        self.location_changed = false;
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.schedule_first_response(vec![self.stat_byte()]);
        // Redux's seek-complete interrupt clears STATUS_SEEK before
        // publishing the second response (`playInterrupt`), so the
        // BIOS observes the motor/rotating bit only once the seek is
        // done. Returning SEEK here makes the license boot path think
        // the drive is still unsettled.
        let stat = self.stat_byte() & !drive_status_bit::SEEKING;
        // Charge the measured mech curve for the actual head travel. The
        // console answers SeekL in ~11 ms even for a 1-sector hop (record
        // 0x90), so there is no free repeat-seek path; the old
        // seek_done-gated 0x800 quick ack made records 0x90/0x91 read as
        // instant where silicon takes 11-79 ms. The head moves: commit the
        // target so a following ReadN does not charge the journey twice.
        let target_lba = {
            let (m, s, f) = self.setloc_msf;
            msf_to_lba(m, s, f)
        };
        let delay = seek_cycles(target_lba.abs_diff(self.read_lba));
        self.read_lba = target_lba;
        self.setloc_pending = false;
        self.schedule_second_response(vec![stat], delay);
        self.seek_done = true;
    }

    fn cmd_read(&mut self) {
        if !self.disc_present {
            // No disc: two-phase response like the other error-returning
            // commands. First an ack (INT3) with stat so the BIOS
            // confirms we got the command, then an error (INT5) a bit
            // later with stat|ERROR + error code 0x80 (shell open /
            // no disc). Sending only the error IRQ confuses the BIOS's
            // command-state machine which expects the ack first.
            self.schedule_first_response(vec![self.stat_byte()]);
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_second_error(vec![stat, 0x80], QUICK_SECOND_RESPONSE_CYCLES);
            return;
        }
        self.cancel_pending_data_ready_events();
        // Starting a read resets the sector buffers. Anything the previous
        // read left behind belongs to a position software has just moved away
        // from, and handing it over would answer the new read with old data.
        // The single-buffer model got this free by overwriting on every
        // sector; a ring has to be told.
        self.clear_data_fifo();
        self.halt_cdda();
        self.cdda_sample_index = 0;
        self.reading = true;
        self.read_rescheduled = false;
        self.seek_done = true;
        self.xa_first_sector = 1;
        self.schedule_first_response(vec![self.stat_byte()]);
        // A pending SetLoc means the head still has to travel before the
        // first sector can stream. Charge the measured mech curve for that
        // distance up front, on top of the ordinary first-sector delay.
        // (The old model instead multiplied the SECOND sector's delay by a
        // flat 30, i.e. ~400 ms at single speed regardless of distance;
        // the console delivers 8 sectors of a near re-seek in ~105 ms,
        // records 0x94/0x95.)
        let mut travel = 0;
        if self.setloc_pending {
            let (m, s, f) = self.setloc_msf;
            let target = msf_to_lba(m, s, f);
            travel = seek_cycles(target.abs_diff(self.read_lba));
            self.read_lba = target;
            self.setloc_pending = false;
            self.location_changed = true;
        } else if self.read_lba == 0 {
            let (m, s, f) = self.setloc_msf;
            self.read_lba = msf_to_lba(m, s, f);
        }
        // Redux arms the first ReadN/ReadS sector from inside the
        // command ACK handler (`interrupt()`), not from the original
        // command write. Chaining it off the ACK keeps the first
        // DataReady deadline anchored on the actual ACK service cycle
        // rather than `scheduling_cycle`, which otherwise lands the
        // first sector ~0x800 cycles too early and makes a commercial title service
        // a CDROM IRQ before Redux does.
        self.chain_followup(
            IrqType::DataReady,
            vec![self.stat_byte()],
            self.initial_sector_read_cycles().saturating_add(travel),
        );
    }

    fn schedule_sector_event_at(&mut self, base_cycle: u64, delay: u64) {
        let stat = self.stat_byte();
        self.insert_pending_event(PendingEvent {
            command: 0x06,
            deadline: base_cycle.saturating_add(delay),
            irq: IrqType::DataReady,
            bytes: vec![stat],
            followup: None,
        });
        self.sector_events_scheduled = self.sector_events_scheduled.saturating_add(1);
    }

    /// On DataReady event firing: populate the data FIFO with the
    /// next sector's user data and bump the read LBA. When the
    /// sector's subheader marks it as an XA audio block and mode
    /// bit 6 (XA ADPCM enable) is set, we ALSO decode the audio
    /// half into `cd_audio` for the SPU's CD input. Called from
    /// `tick` once per sector event.
    ///
    /// Returns whether this sector should raise the CPU-visible
    /// DataReady IRQ. Redux suppresses DataReady for XA audio sectors
    /// while STRSND is enabled, but still schedules the next sector.
    fn load_next_sector(&mut self) -> bool {
        let lba = self.read_lba;
        self.read_lba = self.read_lba.wrapping_add(1);
        if let Some(disc) = self.disc.as_ref() {
            if let Some(raw) = disc.read_sector_raw(lba) {
                // A data read (ReadN/ReadS) that advances into a Red Book
                // audio track delivers no data and raises no DataReady: the
                // sector is skipped because the drive is not playing it. The
                // stream still advances, so the caller schedules the next
                // sector. Games keep their data in track 1, so this only
                // bites a read that runs off the end of the data track.
                if disc.track_for_lba(lba).map(|track| track.track_type)
                    == Some(psx_iso::TrackType::Audio)
                {
                    self.data_fifo.clear();
                    self.data_fifo_ready = false;
                    self.data_transfer_active = false;
                    return false;
                }
                self.last_sector_header.copy_from_slice(&raw[12..16]);
                self.last_sector_subheader.copy_from_slice(&raw[16..20]);
                self.last_sector_header_valid = true;
                let submode = raw[18];
                let suppress_data_ready = self.mode & 0x40 != 0 && submode & 0x04 != 0;
                if suppress_data_ready {
                    self.dbg_suppressed_submode_or |= submode;
                }

                // If XA mode is on, only decode sectors that match the
                // Redux gate: unmuted, audio submode set, matching
                // file/channel filter, and a live first-sector state.
                // Games with XA-streamed cutscenes use a single ReadN
                // to pull both sector kinds; matching audio sectors go
                // to the SPU and skip the CPU-visible data FIFO.
                if !self.muted && self.mode & 0x40 != 0 && self.xa_first_sector != -1 {
                    let file = raw[16];
                    let channel = raw[17];

                    if self.xa_first_sector == 1 && self.mode & 0x08 == 0 {
                        self.xa_filter_file = file;
                        self.xa_filter_channel = channel;
                    }

                    if submode & 0x04 != 0
                        && file == self.xa_filter_file
                        && channel == self.xa_filter_channel
                        && channel != 0xFF
                    {
                        if self.xa_first_sector == 1 || self.xa_coding.is_none() {
                            let Some(coding) = parse_xa_coding(raw[19]) else {
                                self.xa_first_sector = -1;
                                return !suppress_data_ready;
                            };
                            if self.xa_coding != Some(coding) {
                                self.xa_left.reset();
                                self.xa_right.reset();
                                self.xa_coding = Some(coding);
                            }
                        }
                        let coding = self.xa_coding.expect("XA coding seeded above");
                        if let Some(mut samples) = decode_xa_audio_sector(
                            raw,
                            coding,
                            &mut self.xa_left,
                            &mut self.xa_right,
                        ) {
                            self.attenuate_cd_samples(&mut samples);
                            self.append_cd_audio_samples(&samples);
                            self.xa_first_sector = 0;
                            self.data_fifo.clear();
                            self.data_fifo_ready = false;
                            self.data_transfer_active = false;
                            return self.suppress_data_ready();
                        }
                        self.xa_first_sector = -1;
                    }
                }

                let whole_sector = self.mode & 0x20 != 0;
                let sector_mode = raw[15];
                let payload = if whole_sector {
                    &raw[12..12 + 2340]
                } else if sector_mode == 1 {
                    &raw[16..16 + 2048]
                } else {
                    &raw[24..24 + 2048]
                };
                self.push_sector(lba, payload.to_vec());
                if suppress_data_ready {
                    return self.suppress_data_ready();
                }
                return true;
            }

            // Raw-sector miss means we ran off the end of the image.
        }
        // Past end of disc -- stop the read and leave the FIFO empty.
        self.reading = false;
        self.read_rescheduled = false;
        self.location_changed = false;
        self.data_fifo.clear();
        self.data_fifo_ready = false;
        self.data_transfer_active = false;
        true
    }

    /// CdlGetlocL (0x10) -- return the current logical position
    /// and sector-header info. 8-byte reply:
    /// `[MM, SS, FF, Mode, File, Channel, Submode, Coding]` from the
    /// last delivered sector's raw header/subheader.
    ///
    /// Without a disc, returns an INT5 error like GetID.
    fn cmd_get_loc_l(&mut self) {
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        if !self.last_sector_header_valid {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        }
        let mut resp = Vec::with_capacity(8);
        resp.extend_from_slice(&self.last_sector_header);
        resp.extend_from_slice(&self.last_sector_subheader);
        self.schedule_first_response(resp);
    }

    /// CdlGetlocP (0x11) -- return the current physical play
    /// position. 8-byte reply: `[Track, Index, RMM, RSS, RSECT,
    /// AMM, ASS, ASECT]`. RMM/RSS/RSECT are relative to the
    /// track/index start; AMM/ASS/ASECT are absolute MSF.
    fn cmd_get_loc_p(&mut self) {
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        let Some(disc) = self.disc.as_ref() else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        let lba = if self.reading {
            self.read_lba.saturating_sub(1)
        } else {
            self.read_lba
        };
        let Some(pos) = disc.track_position_for_lba(lba) else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        let (rm, rs, rf) = pos.relative_msf;
        let (am, as_, af) = pos.absolute_msf;
        self.schedule_first_response(vec![
            bin_to_bcd(pos.track_number),
            pos.index_number,
            bin_to_bcd(rm),
            bin_to_bcd(rs),
            bin_to_bcd(rf),
            bin_to_bcd(am),
            bin_to_bcd(as_),
            bin_to_bcd(af),
        ]);
    }

    /// CdlGetTN (0x13) -- first and last track numbers from the disc's
    /// track table.
    fn cmd_get_tn(&mut self) {
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        let Some(disc) = self.disc.as_ref() else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        let (Some(first), Some(last)) = (disc.first_track_number(), disc.last_track_number())
        else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        self.schedule_first_response(vec![self.stat_byte(), bin_to_bcd(first), bin_to_bcd(last)]);
    }

    /// CdlGetTD (0x14) -- start time for a given track, or lead-out for
    /// track 0. Parameter is a BCD track number.
    fn cmd_get_td(&mut self, params: &[u8]) {
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        let Some(disc) = self.disc.as_ref() else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        let track = bcd_to_bin(params.first().copied().unwrap_or(0));
        if track == 0xFF {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        }
        let target_lba = if track == 0 {
            disc.leadout_lba()
        } else {
            let Some(start_lba) = disc.track_start_lba(track) else {
                let stat = self.stat_byte() | drive_status_bit::ERROR;
                self.schedule_error_response(vec![stat]);
                return;
            };
            start_lba
        };
        let (m, s, _f) = lba_to_msf(target_lba);
        // PCSX-Redux's CdlGetTD path calls SetResultSize(4) but only
        // writes stat/min/sec, leaving the fourth result byte at the
        // controller's zero-initialized slot. The BIOS CDROM handler
        // drains all four bytes by polling status bit 5; publishing
        // only three makes it skip one poll/drain loop and breaks
        // cycle parity in a commercial boot path.
        self.schedule_first_response(vec![self.stat_byte(), bin_to_bcd(m), bin_to_bcd(s), 0x00]);
    }

    /// Legacy helper kept for the `0x04` / `0x05` forward / backward
    /// commands which still use "ack or nodisc-error" semantics.
    #[allow(dead_code)]
    fn cmd_simple_stat_or_nodisc(&mut self) {
        if self.disc_present {
            self.schedule_first_response(vec![self.stat_byte()]);
        } else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
        }
    }

    /// CdlPlay (0x03) -- CD-DA playback. Parameter is an optional
    /// track number (BCD); when absent, playback continues from
    /// the last SetLoc position.
    fn cmd_play(&mut self, params: &[u8]) {
        // Where the head is now, before any of the branches below move it to
        // the requested track. The distance between the two is what the seek
        // costs.
        let from_lba = self.read_lba;
        if !self.disc_present {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat, 0x80]);
            return;
        }
        let Some(disc) = self.disc.as_ref() else {
            let stat = self.stat_byte() | drive_status_bit::ERROR;
            self.schedule_error_response(vec![stat]);
            return;
        };
        if let Some(&track_bcd) = params.first() {
            let track = bcd_to_bin(track_bcd);
            if track == 0xFF {
                let stat = self.stat_byte() | drive_status_bit::ERROR;
                self.schedule_error_response(vec![stat]);
                return;
            }
            if track != 0 {
                let Some(start_lba) = disc.track_start_lba(track) else {
                    let stat = self.stat_byte() | drive_status_bit::ERROR;
                    self.schedule_error_response(vec![stat]);
                    return;
                };
                self.read_lba = start_lba;
                self.setloc_pending = false;
            }
        } else if self.setloc_pending || self.setloc_msf != (0, 0, 0) {
            let (m, s, f) = self.setloc_msf;
            self.read_lba = msf_to_lba(m, s, f);
            self.setloc_pending = false;
        } else if self.read_lba == 0 {
            self.read_lba = disc
                .track_start_lba(disc.first_track_number().unwrap_or(1))
                .unwrap_or(0);
        }
        self.motor_on = true;
        self.reading = false;
        self.read_rescheduled = false;
        self.cancel_pending_data_ready_events();
        self.clear_data_fifo();
        self.reset_xa_stream();
        self.cdda_sample_index = 0;
        // Play is a seek followed by playback, not playback. Until the head
        // arrives the drive reports SEEKING with the playing bit CLEAR, and
        // decodes no samples. Guest code that reads "not playing" as "the
        // track finished" depends on this being modelled: without it every
        // such poll answers correctly by accident.
        self.drive_status &= !drive_status_bit::PLAYING;
        self.drive_status |= drive_status_bit::SEEKING;
        self.cdda_seek_done_at = Some(
            self.scheduling_cycle
                .saturating_add(seek_cycles(from_lba.abs_diff(self.read_lba))),
        );
        self.schedule_first_response(vec![self.stat_byte()]);
    }

    /// Land an in-flight Play: the head has reached the track, so the drive
    /// stops seeking and starts producing audio.
    fn complete_cdda_seek(&mut self, cycles_now: u64) {
        let Some(done_at) = self.cdda_seek_done_at else {
            return;
        };
        if cycles_now < done_at {
            return;
        }
        self.cdda_seek_done_at = None;
        self.drive_status &= !drive_status_bit::SEEKING;
        self.drive_status |= drive_status_bit::PLAYING;
    }

    fn cmd_test(&mut self, params: &[u8]) {
        // Only Test 0x20 (drive version / BIOS date) is commonly used
        // by the BIOS. Must match Redux byte-for-byte -- the BIOS's
        // IRQ handler stores the 4-byte response into a kernel
        // buffer, and later code paths read those bytes back to
        // dispatch on firmware version. Parity step 89,184,517
        // diverged on a byte out of this buffer.
        //
        // Redux (cdrom.cc): `Test20[] = {0x98, 0x06, 0x10, 0xC3}`.
        // Format is YY MM DD VER -- 1998-06-10 v0xC3, matching the
        // SCPH-550x / 700x firmware Redux targets by default.
        match params.first().copied() {
            Some(0x20) => {
                self.schedule_first_response(vec![0x98, 0x06, 0x10, 0xC3]);
            }
            _ => self.cmd_getstat(),
        }
    }

    fn cmd_getid(&mut self) {
        if self.disc_present {
            // Match PCSX-Redux's BIOS-visible response exactly:
            // stat, licensed flags clear, two reserved zeros, then a
            // benign four-byte controller ID. The BIOS has already
            // verified the region/license string by reading the early
            // data sectors; returning SCEA/SCEE here made every local
            // disc reach the license screen and then fall back to the
            // shell's repeated Init loop instead of continuing into the
            // filesystem boot path.
            let stat = self.stat_byte();
            self.schedule_first_response(vec![stat]);
            self.schedule_second_response(
                vec![stat, 0x00, 0x00, 0x00, b'P', b'C', b'S', b'X'],
                GETID_SECOND_RESPONSE_CYCLES,
            );
        } else {
            // No disc: 1st response (INT3) with stat, 2nd response
            // (INT5, Error) with the shell-recognised "no disc"
            // pattern. The second response MUST be Error, not
            // Complete - the BIOS dispatches on irq_flag and runs a
            // different code path for INT5 that transitions the shell
            // state machine from "probing" to "show insert-disc screen".
            self.schedule_first_response(vec![self.stat_byte()]);
            self.schedule_second_error(
                vec![0x08, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                GETID_SECOND_RESPONSE_CYCLES,
            );
        }
    }

    /// Advance pending events by `cycles_now` (absolute bus cycle
    /// count). Events whose deadlines are in the past deliver their
    /// bytes into the response FIFO and raise their IRQ type (stored
    /// in `irq_flag`; actual CPU-facing wake-up happens via the IRQ
    /// controller, which the caller raises when `irq_flag` transitions
    /// non-zero).
    ///
    /// Returns `true` if this call raised an IRQ that was previously
    /// clear -- the caller (Bus) uses that to poke `IrqSource::Cdrom`.
    pub fn tick(&mut self, cycles_now: u64) -> bool {
        self.tick_with_irq_pending(cycles_now, false)
    }

    /// Variant used by the full bus: Redux's CD read interrupt also
    /// sees the CPU interrupt controller and applies one extra sector
    /// delay if the CDROM IRQ bit is still pending there.
    pub fn tick_with_irq_pending(&mut self, cycles_now: u64, cdrom_irq_pending: bool) -> bool {
        let mut raised = false;

        self.tick_lid_state_machine(cycles_now);
        self.complete_cdda_seek(cycles_now);

        while let Some(front) = self.pending.front() {
            // Redux's scheduled interrupt queue only dispatches when
            // `target < cycle` (see `R3000Acpu::branchTest`). CDROM
            // responses live on that queue, unlike root counters /
            // VBlank which update on equality. Keep CDROM strict so a
            // BIOS poll on the exact target cycle still sees no IRQ.
            if front.deadline >= cycles_now {
                break;
            }

            // Check IRQ-flag gate BEFORE popping or running the
            // event. Hardware holds a latched IRQ until software
            // acks via 0x1F801803 idx=1; if we already have an
            // unacked IRQ, leave the front event in the queue.
            //
            // If we popped first and then re-queued on flag-set
            // (as the previous implementation did), we'd also run
            // the event's side effects (notably
            // `load_next_sector` + chained
            // `schedule_sector_event_at`) on every failed attempt.
            // a commercial title triggered that tight loop: 46.9 M sector events
            // scheduled and 11.7 M DataReady pops -- enough
            // load_next_sector re-entries to bury the emulator in
            // ISR dispatch cycles (16.7 cyc/step vs 2.4 baseline),
            // stranding a commercial title at the PlayStation splash.
            //
            // This holds COMMAND responses only. There is one response FIFO,
            // so hardware will not overwrite a packet software has not read.
            // Sector delivery is not like that: the disc is turning and the
            // drive reads at its own rate whatever software is doing. Holding
            // sectors here made a late interrupt handler cost nothing, when on
            // silicon it costs the sectors that arrived meanwhile. Let
            // DataReady through and let `push_sector` drop what overflows.
            if self.irq_flag != 0 && front.irq != IrqType::DataReady {
                // Bump the front event's deadline slightly so the
                // next tick re-checks, rather than spinning on an
                // already-due event every tick until the ack lands.
                // Matches Redux's `irqReschedule` cadence exactly
                // without ever re-running the event body.
                let delay = IRQ_RESCHEDULE_CYCLES;
                if let Some(mut ev) = self.pending.pop_front() {
                    ev.deadline = cycles_now.saturating_add(delay);
                    self.insert_pending_event(ev);
                }
                break;
            }
            // The same reasoning applies to the CPU's own interrupt line: a
            // handler that has not run yet does not slow the disc down. The
            // sector lands on time and takes its chances in the ring.
            let _ = cdrom_irq_pending;

            let ev = self.pending.pop_front().unwrap();

            // If the drive was paused/reset between this event's
            // scheduling and now, drop it silently. On real
            // hardware, Pause/Init kills in-flight sector reads;
            // letting a stale DataReady fire would clobber the
            // data FIFO that software was about to drain, or
            // deliver the wrong LBA entirely.
            if ev.irq == IrqType::DataReady && !self.reading {
                continue;
            }

            // DataReady events drive the sector-stream -- load the
            // next sector's payload into the data FIFO as the event
            // fires, and chain the subsequent DataReady (anchored on
            // `cycles_now` so the next sector fires at the steady
            // stream cadence after the PREVIOUS one, not from the
            // ancient `cmd_read` issue time).
            let mut should_raise_irq = true;
            if ev.irq == IrqType::DataReady {
                should_raise_irq = self.load_next_sector();
                // The sector has landed either way. Whether the CPU is told
                // about it is a separate question: the interrupt line is still
                // holding the previous, unacknowledged one, and hardware will
                // not stack a second. The sector waits in the ring and its
                // notification is simply never sent, which is exactly how a
                // slow handler ends up stepping over data.
                if self.irq_flag != 0 {
                    should_raise_irq = false;
                }
                self.read_rescheduled = false;
                if self.reading {
                    // The head-travel cost of a moved SetLoc is charged on
                    // the FIRST sector (see cmd_read); once sectors stream,
                    // silicon delivers them at the nominal frame cadence
                    // (records 0x94/0x95: 8 sectors in ~105 ms single /
                    // ~52 ms double, first included). The old flat x30 here
                    // put a ~400 ms cliff on the second sector instead.
                    self.location_changed = false;
                    self.schedule_sector_event_at(cycles_now, self.sector_read_cycles());
                }
            }
            if !should_raise_irq {
                continue;
            }
            // Like Redux's `m_result`, each IRQ publishes a fresh
            // packet rather than appending to any unread prior bytes.
            self.responses.clear();
            for b in ev.bytes.iter().copied() {
                if self.responses.len() < RESPONSE_FIFO_DEPTH {
                    self.responses.push_back(b);
                }
            }
            if self.response_log.len() < self.response_log_cap {
                let mut bytes = [0u8; RESPONSE_FIFO_DEPTH];
                let n = ev.bytes.len().min(RESPONSE_FIFO_DEPTH);
                bytes[..n].copy_from_slice(&ev.bytes[..n]);
                self.response_log.push(CdRomResponseLogEntry {
                    cycle: cycles_now,
                    irq: ev.irq,
                    bytes,
                    len: n as u8,
                });
            }
            self.command_busy = false;
            // Raise IRQ. The flag-gate above already guaranteed
            // irq_flag was 0 on entry.
            if ev.irq == IrqType::DataReady {
                self.snap_to_newest_sector();
            }
            self.irq_flag = ev.irq as u8;
            let ty = ev.irq as usize;
            if ty < self.irq_type_counts.len() {
                self.irq_type_counts[ty] = self.irq_type_counts[ty].saturating_add(1);
            }
            // Per-raise log for divergence probes. Cap-guarded so
            // long production runs don't bloat memory.
            if self.cdrom_irq_log.len() < self.cdrom_irq_log_cap {
                self.cdrom_irq_log.push((cycles_now, ev.irq as u8));
            }
            if let Some(followup) = ev.followup {
                self.insert_pending_event(PendingEvent {
                    command: followup.command,
                    deadline: cycles_now.saturating_add(followup.delay),
                    irq: followup.irq,
                    bytes: followup.bytes,
                    followup: None,
                });
            }
            if self.lid_bootstrap_pending && ev.irq == IrqType::Acknowledge {
                self.lid_bootstrap_pending = false;
                self.schedule_lid_transition_at(cycles_now, LID_BOOTSTRAP_CYCLES);
            }
            raised = true;
            // Hardware can only latch one IRQ at a time; subsequent
            // due events wait until this one is acked.
            break;
        }

        raised
    }

    /// Total commands received -- used by `smoke_draw` to confirm BIOS
    /// is talking to the drive.
    pub fn commands_dispatched(&self) -> u64 {
        self.commands_dispatched
    }

    /// Per-command histogram (indexed by command byte) -- same purpose.
    pub fn command_histogram(&self) -> &[u32; 32] {
        &self.command_hist
    }

    /// Enable bounded command logging for diagnostics.
    pub fn enable_command_log(&mut self, cap: usize) {
        self.command_log_cap = cap;
        self.command_log.clear();
    }

    /// Captured command-port writes, up to the configured cap.
    pub fn command_log(&self) -> &[CdRomCommandLogEntry] {
        &self.command_log
    }

    /// Enable bounded response-packet logging for diagnostics.
    pub fn enable_response_log(&mut self, cap: usize) {
        self.response_log_cap = cap;
        self.response_log.clear();
    }

    /// Captured response IRQ packets, up to the configured cap.
    pub fn response_log(&self) -> &[CdRomResponseLogEntry] {
        &self.response_log
    }

    /// Current raw IRQ-flag (for diagnostics).
    pub fn irq_flag(&self) -> u8 {
        self.irq_flag
    }

    /// Current CDROM controller INDEX (bits 0-1 of the status
    /// register at 0x1F801800). Low-level writes to 0x1F801801-3
    /// are routed through this index -- reading 0x1F801803 with
    /// index=0 returns the IRQ mask, with index=1 returns the
    /// IRQ flag. Probes compare this against Redux's to catch
    /// index-tracking drift.
    pub fn index_value(&self) -> u8 {
        self.index
    }

    /// Current CDROM IRQ mask (the per-IRQ-type enable bits -- the
    /// CPU-level I_MASK is separate). Written via 0x1F801802
    /// index=1. `setIrq` in Redux (and our raise-gate) checks
    /// `irq_flag & irq_mask` before waking the CPU.
    pub fn irq_mask_value(&self) -> u8 {
        self.irq_mask
    }

    /// Redux-equivalent `setIrq()` gate: the CDROM only escalates
    /// a latched IRQ to the PSX IRQ controller (I_STAT bit 2) when
    /// `irq_flag & irq_mask` is nonzero. When it's zero the
    /// response stays latched for polled access via 0x1F801803
    /// idx=1, but no CPU interrupt is dispatched. a commercial title (and other
    /// games) poll the flag with bits 0-2 of `irq_mask` cleared --
    /// relying on this gate to keep CDROM acks from firing the
    /// ISR while the BIOS's loader code walks the response
    /// manually. Skipping the gate (our pre-fix behaviour) caused
    /// the BIOS to run an ISR it didn't expect, stomping state
    /// the a commercial title boot loop needed.
    pub fn should_wake_cpu(&self) -> bool {
        (self.irq_flag & self.irq_mask) != 0
    }

    /// Number of pending events queued. 0 means the CDROM has
    /// nothing scheduled to fire.
    pub fn pending_queue_len(&self) -> usize {
        self.pending.len()
    }

    /// Diagnostic snapshot of the read/IRQ state machine relative to `now`
    /// (the bus cycle), used to debug stuck CD completions.
    pub fn debug_state(&self, now: u64) -> String {
        let pend: Vec<String> = self
            .pending
            .iter()
            .map(|e| {
                let due = e.deadline as i64 - now as i64;
                format!(
                    "{:?}(cmd{:02x},dl={},due{:+})",
                    e.irq, e.command, e.deadline, due
                )
            })
            .collect();
        format!(
            "reading={} mode={:02x} muted={} xa_first={} read_resched={} loc_changed={} \
cmd_busy={} dr_suppressed={} submode_or={:02x} last_subhdr=[{:02x} {:02x} {:02x} {:02x}] \
xa_filter=({},{}) sched_cycle={} read_lba={} now={} pending=[{}]",
            self.reading,
            self.mode,
            self.muted,
            self.xa_first_sector,
            self.read_rescheduled,
            self.location_changed,
            self.command_busy,
            self.data_ready_suppressed,
            self.dbg_suppressed_submode_or,
            self.last_sector_subheader[0],
            self.last_sector_subheader[1],
            self.last_sector_subheader[2],
            self.last_sector_subheader[3],
            self.xa_filter_file,
            self.xa_filter_channel,
            self.scheduling_cycle,
            self.read_lba,
            now,
            pend.join(", ")
        )
    }

    /// Absolute cycle used as the base for the most recent command's
    /// response scheduling. Diagnostic-only.
    pub fn scheduling_cycle(&self) -> u64 {
        self.scheduling_cycle
    }

    /// Front pending event as `(deadline, irq_type)`. Lets probes
    /// compare the next latched CDROM action against Redux without
    /// exposing the full private queue.
    pub fn next_pending_event(&self) -> Option<(u64, IrqType)> {
        self.pending.front().map(|ev| (ev.deadline, ev.irq))
    }

    /// Enable per-raise logging up to `cap` entries. Probes call
    /// this once before running; afterward, read `cdrom_irq_log`.
    pub fn enable_irq_log(&mut self, cap: usize) {
        self.cdrom_irq_log_cap = cap;
        self.cdrom_irq_log.reserve(cap);
    }

    /// Current IRQ-enable mask (for diagnostics).
    pub fn irq_mask_raw(&self) -> u8 {
        self.irq_mask
    }

    /// The most-recently-dispatched command byte (for diagnostics).
    pub fn last_command(&self) -> u8 {
        self.last_command
    }

    /// Most-recent `SetLoc` MSF target, as a `(minute, second,
    /// frame)` BCD triple. Diagnostic-only -- lets probes correlate
    /// `ReadN` events with the LBA the BIOS is asking for.
    pub fn debug_setloc_msf(&self) -> (u8, u8, u8) {
        self.setloc_msf
    }

    /// Total bytes popped from the data FIFO via MMIO reads since
    /// boot. Diagnostic.
    pub fn data_fifo_pops(&self) -> u64 {
        self.data_fifo_pops
    }

    /// Number of sector events consumed without a CPU-visible
    /// DataReady IRQ since reset.
    pub fn data_ready_suppressed(&self) -> u64 {
        self.data_ready_suppressed
    }

    /// `true` when the request register's bit-7 latch has armed the
    /// current sector buffer for MMIO/DMA consumption.
    pub fn data_transfer_armed(&self) -> bool {
        self.data_transfer_active
    }

    /// `true` when the sector-buffer status bit is currently set.
    /// Diagnostic-only; the buffered byte count can remain nonzero
    /// after an unarmed data-port read drops the ready latch.
    pub fn data_fifo_ready(&self) -> bool {
        self.data_fifo_ready
    }

    /// Pull one byte from the data FIFO -- used by DMA channel 3's
    /// block-read path to drain a sector into RAM. Returns `0` when
    /// the FIFO is empty (hardware returns stale-bus bytes; `0` is
    /// a safe stand-in).
    pub fn pop_data_byte(&mut self) -> u8 {
        self.data_fifo_pops = self.data_fifo_pops.saturating_add(1);
        self.pop_data_fifo_byte()
    }

    /// Pull one byte for DMA3. Redux gates CDROM DMA on the sector
    /// ready flag (`m_read`), not on the request-register transfer
    /// latch that controls MMIO reads, so DMA drains the buffered
    /// sector directly.
    pub fn pop_dma_data_byte(&mut self) -> u8 {
        self.data_fifo_pops = self.data_fifo_pops.saturating_add(1);
        let byte = self.data_fifo.pop_front().unwrap_or(0);
        if self.data_fifo.is_empty() {
            self.data_transfer_active = false;
            self.advance_to_next_sector();
        }
        byte
    }

    /// Sectors the controller read but software never collected, because it
    /// fell further behind than the buffer ring is deep. Silent to the guest,
    /// so surfacing it here is the only way anyone finds out.
    pub fn dropped_sectors(&self) -> u64 {
        self.dropped_sectors
    }

    /// Disc positions of the first and last loss, so the read responsible can
    /// be identified from the pack layout.
    pub fn dropped_lba_range(&self) -> (u32, u32) {
        (self.dropped_lba_first, self.dropped_lba_last)
    }

    pub fn data_fifo_len(&self) -> usize {
        self.data_fifo.len()
    }

    /// Current sector-buffer length expressed as DMA words. Used when
    /// software programs a zero-sized CDROM DMA and expects the drive
    /// to fall back to the active sector size (2048/2340 bytes).
    pub fn data_fifo_words(&self) -> u32 {
        self.data_fifo.len().div_ceil(4) as u32
    }

    #[cfg(test)]
    pub(crate) fn debug_seed_data_fifo(&mut self, bytes: &[u8], ready: bool, armed: bool) {
        self.data_fifo.clear();
        self.data_fifo.extend(bytes.iter().copied());
        self.data_fifo_ready = ready;
        self.data_transfer_active = armed;
    }
}

impl Default for CdRom {
    fn default() -> Self {
        Self::new()
    }
}

// Shared helpers -- `lba_to_msf` + `bin_to_bcd` live in `psx-iso` so
// any crate that speaks the CDROM protocol (tools, test harnesses)
// can use them.
use psx_iso::{bin_to_bcd, lba_to_msf};

#[cfg(test)]
fn disc_region_code(disc: &Disc) -> [u8; 4] {
    let Some(user) = disc.read_sector_user(4) else {
        return *b"SCEA";
    };
    let text = String::from_utf8_lossy(user);
    if text.contains("Sony Computer Entertainment Amer") {
        *b"SCEA"
    } else if text.contains("Sony Computer Entertainment Euro")
        || text.contains("Sony Computer Entertainment Inc. for U.K.")
    {
        *b"SCEE"
    } else if text.contains("Sony Computer Entertainment Inc.") {
        *b"SCEI"
    } else {
        *b"SCEA"
    }
}

/// Decode one raw 2352-byte Mode 2 Form 2 XA ADPCM audio sector into
/// stereo PCM samples. Returns `None` when the sector isn't an XA
/// audio sector (subheader submode bit 2 / Form 2 bit 5 not set).
///
/// Sector layout (Mode 2):
/// - 0..=11   : sync pattern (0x00, 12× 0xFF, 0x00)
/// - 12..=14  : MSF header
/// - 15       : mode (02 for Mode 2)
/// - 16..=23  : 8-byte subheader (4 bytes × 2 copies)
/// - 24..=2347: 2324-byte user data (XA audio payload for Form 2)
/// - 2348+    : EDC (unused for Form 2)
///
/// Subheader byte 2 (submode): bit 2 = audio, bit 5 = Form 2.
/// Subheader byte 3 (coding info): bits 0-1 mono/stereo, bits 2-3
/// sample rate, bits 4-5 bits/sample.
///
/// Decodes the Redux-supported XA layouts: 4-bit/8-bit, mono/stereo,
/// at 37.8 kHz or 18.9 kHz. Unsupported coding nibbles return `None`.
///
/// Samples are decoded in Redux's sound-unit order, then resampled from
/// the XA source rate up to the SPU's 44.1 kHz rate on output.
fn parse_xa_coding(coding: u8) -> Option<XaCoding> {
    let stereo = match coding & 0x03 {
        1 => true,
        0 => false,
        _ => return None,
    };
    let freq = match (coding >> 2) & 0x03 {
        0 => 37_800u32,
        1 => 18_900u32,
        _ => return None,
    };
    let nbits = match (coding >> 4) & 0x03 {
        0 => 4u8,
        1 => 8u8,
        _ => return None,
    };
    Some(XaCoding {
        stereo,
        freq,
        nbits,
    })
}

fn decode_xa_audio_sector(
    raw: &[u8],
    coding: XaCoding,
    left: &mut crate::spu::XaDecoderState,
    right: &mut crate::spu::XaDecoderState,
) -> Option<Vec<(i16, i16)>> {
    if raw.len() < 2352 {
        return None;
    }
    let stereo = coding.stereo;
    let freq = coding.freq;
    let nbits = coding.nbits;

    // XA payload starts at offset 24 (after 12+4+8 bytes of header).
    // 18 sound groups × 128 bytes. 4-bit stereo yields 2016 source
    // frames, while 4-bit mono yields 4032. 8-bit modes carry fewer
    // sound units per group; keep the same Redux unpacking path rather
    // than rejecting the stream and prematurely killing playback.
    let payload = &raw[24..24 + 18 * 128];
    let units_per_group = if nbits == 4 { 4 } else { 2 };
    let mut decoded: Vec<(i16, i16)> =
        Vec::with_capacity(18 * units_per_group * 28 * if stereo { 1 } else { 2 });
    let head_table = [0usize, 2, 8, 10];
    for group_idx in 0..18 {
        let group = &payload[group_idx * 128..group_idx * 128 + 128];
        let headers = &group[0..16];
        let data = &group[16..128];

        for unit in 0..units_per_group {
            let decode_words = if nbits == 4 {
                let mut low_words = [0u16; 7];
                let mut high_words = [0u16; 7];
                for k in 0..7 {
                    let base = k * 16 + unit;
                    let b0 = data[base] as u16;
                    let b1 = data[base + 4] as u16;
                    let b2 = data[base + 8] as u16;
                    let b3 = data[base + 12] as u16;
                    low_words[k] =
                        (b0 & 0x0F) | ((b1 & 0x0F) << 4) | ((b2 & 0x0F) << 8) | ((b3 & 0x0F) << 12);
                    high_words[k] =
                        (b0 >> 4) | ((b1 >> 4) << 4) | ((b2 >> 4) << 8) | ((b3 >> 4) << 12);
                }
                (low_words, high_words)
            } else {
                let mut words = [0u16; 7];
                for (k, word) in words.iter_mut().enumerate() {
                    let base = k * 8 + unit;
                    *word = data[base] as u16 | ((data[base + 4] as u16) << 8);
                }
                (words, words)
            };

            let mut first_samples = [0i16; 28];
            crate::spu::xa_decode_block(
                left,
                headers[head_table[unit]],
                &decode_words.0,
                &mut first_samples,
                1,
            );

            if stereo {
                let mut second_samples = [0i16; 28];
                crate::spu::xa_decode_block(
                    right,
                    headers[head_table[unit] + 1],
                    &decode_words.1,
                    &mut second_samples,
                    1,
                );
                for i in 0..28 {
                    decoded.push((first_samples[i], second_samples[i]));
                }
            } else {
                for &sample in &first_samples {
                    decoded.push((sample, sample));
                }
                let mut second_samples = [0i16; 28];
                crate::spu::xa_decode_block(
                    left,
                    headers[head_table[unit] + 1],
                    &decode_words.1,
                    &mut second_samples,
                    1,
                );
                for &sample in &second_samples {
                    decoded.push((sample, sample));
                }
            }
        }
    }

    // Upsample to the SPU rate. This is still a simple resampler, but
    // the sector decode above now matches Redux's sound-unit ordering
    // and frame count.
    let mut resampled: Vec<(i16, i16)> =
        Vec::with_capacity(decoded.len() * 44_100 / freq as usize + 1);
    let src_n = decoded.len() as u32;
    let dst_n = (src_n as u64 * 44_100 / freq as u64) as u32;
    for i in 0..dst_n {
        let src_idx = ((i as u64 * src_n as u64) / dst_n as u64) as usize;
        resampled.push(decoded[src_idx.min(decoded.len() - 1)]);
    }
    Some(resampled)
}

#[cfg(test)]
#[cfg(test)]
mod tests;
