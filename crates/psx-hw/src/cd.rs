//! CD-ROM controller registers, command bytes and status bits.
//!
//! Reference: nocash PSX-SPX "CDROM Controller I/O Ports" and "CDROM Controller
//! Command Summary".

/// Controller register base (index/status register; ports 1..3 follow).
pub const BASE: u32 = 0x1F80_1800;

/// The four byte registers. Which function a register has depends on the
/// index in the low two bits of [`INDEX`](reg::INDEX); only the functions
/// the SDK uses are named.
pub mod reg {
    /// Index/status register: writes select the register bank, reads return
    /// the [`index_status`](super::index_status) bits.
    #[doc(alias = "CD_INDEX")]
    pub const INDEX: u32 = super::BASE;
    /// Index 0: command byte (write), response FIFO (read).
    pub const COMMAND_RESPONSE: u32 = super::BASE + 1;
    /// Index 0: parameter FIFO (write), data FIFO (read).
    pub const PARAMETER: u32 = super::BASE + 2;
    /// Index 0: request register (write). Index 1: interrupt flag (read and
    /// write), see [`irq`](super::irq).
    pub const REQUEST_IRQ: u32 = super::BASE + 3;
}

/// Bits of the index/status register read at [`reg::INDEX`].
pub mod index_status {
    /// Bits 0..=1: the selected register bank.
    pub const INDEX_MASK: u8 = 0b11;
    /// Bit 2: an XA-ADPCM sector is being played (the ADPCM FIFO is not empty).
    pub const ADPCM_BUSY: u8 = 1 << 2;
    /// Bit 3: the parameter FIFO is empty.
    pub const PARAM_EMPTY: u8 = 1 << 3;
    /// Bit 4: the parameter FIFO has room for another byte.
    pub const PARAM_NOT_FULL: u8 = 1 << 4;
    /// Bit 5: the response FIFO holds at least one byte.
    pub const RESPONSE_NOT_EMPTY: u8 = 1 << 5;
    /// Bit 6: the data FIFO holds at least one byte.
    pub const DATA_FIFO_NOT_EMPTY: u8 = 1 << 6;
    /// Bit 7: a command is being transmitted to the drive.
    pub const COMMAND_BUSY: u8 = 1 << 7;
}

/// Interrupt codes and the writes that handle them.
///
/// Reading the interrupt flag register (index 1 of [`reg::REQUEST_IRQ`])
/// returns one of the response codes in its low bits; writing bits to it
/// acknowledges them.
pub mod irq {
    /// INT1: a data sector is ready.
    pub const DATA_READY: u8 = 1;
    /// INT2: a second-response command finished.
    pub const COMPLETE: u8 = 2;
    /// INT3: a command was accepted (first response).
    pub const ACKNOWLEDGE: u8 = 3;
    /// INT4: the end of the data or audio was reached.
    pub const DATA_END: u8 = 4;
    /// INT5: the command failed or the disc is in error.
    pub const ERROR: u8 = 5;
    /// Interrupt flag write that acknowledges every pending code.
    pub const ACK_ALL: u8 = 0x1F;
    /// Interrupt flag write bit that also empties the parameter FIFO.
    pub const CLEAR_PARAMETER_FIFO: u8 = 0x40;
}

/// Bits of the request register (index 0 of [`reg::REQUEST_IRQ`], write).
pub mod request {
    /// Arm the data FIFO so the next sector's bytes can be popped; writing 0
    /// drops it again.
    pub const WANT_DATA: u8 = 0x80;
}

/// Setmode bit: allow CD-DA playback via `Play`.
pub const MODE_CDDA: u8 = 1 << 0;
/// Setmode bit: auto-pause at the end of a CD-DA track. The drive stops on its
/// own at the track boundary (reporting a clear PLAYING bit) instead of running
/// on into the next track / lead-out, so software can detect end-of-track and
/// loop without seeking the laser mid-playback.
pub const MODE_AUTO_PAUSE: u8 = 1 << 1;
/// Setmode bit: emit periodic play-report IRQs.
pub const MODE_REPORT: u8 = 1 << 2;
/// Setmode bit: double-speed data reads.
pub const MODE_DOUBLE_SPEED: u8 = 1 << 7;
/// Setmode bit: send XA-ADPCM sectors to the SPU's CD input. With it set the
/// drive raises no data IRQ for ADPCM sectors, only for the others.
pub const MODE_XA_ADPCM: u8 = 1 << 6;
/// Setmode bit: with [`MODE_XA_ADPCM`], play only the ADPCM sectors whose
/// file and channel match the last `Setfilter`.
pub const MODE_XA_FILTER: u8 = 1 << 3;

/// `Getstat` command (PsyQ `CdlNop`).
#[doc(alias = "Getstat")]
#[doc(alias = "CdlNop")]
pub const CMD_GETSTAT: u8 = 0x01;
/// `Setloc` command.
#[doc(alias = "Setloc")]
#[doc(alias = "CdlSetloc")]
pub const CMD_SETLOC: u8 = 0x02;
/// `Setfilter` command: file and channel number of the XA-ADPCM sectors to
/// play when [`MODE_XA_FILTER`] is set.
#[doc(alias = "Setfilter")]
#[doc(alias = "CdlSetfilter")]
pub const CMD_SETFILTER: u8 = 0x0D;
/// `Play` command.
#[doc(alias = "Play")]
#[doc(alias = "CdlPlay")]
pub const CMD_PLAY: u8 = 0x03;
/// `ReadN` command.
#[doc(alias = "ReadN")]
#[doc(alias = "CdlReadN")]
pub const CMD_READN: u8 = 0x06;
/// `ReadS` command: start a data read without pausing to retry on a bad
/// sector, the usual way to stream XA-ADPCM audio.
#[doc(alias = "ReadS")]
#[doc(alias = "CdlReadS")]
pub const CMD_READS: u8 = 0x1B;
/// `Stop` command.
#[doc(alias = "Stop")]
#[doc(alias = "CdlStop")]
pub const CMD_STOP: u8 = 0x08;
/// `Pause` command.
#[doc(alias = "Pause")]
#[doc(alias = "CdlPause")]
pub const CMD_PAUSE: u8 = 0x09;
/// `Init` command: reset mode, abort pending reads, spin the motor up.
#[doc(alias = "Init")]
#[doc(alias = "CdlInit")]
pub const CMD_INIT: u8 = 0x0A;
/// `Mute` command.
#[doc(alias = "Mute")]
#[doc(alias = "CdlMute")]
pub const CMD_MUTE: u8 = 0x0B;
/// `Demute` command.
#[doc(alias = "Demute")]
#[doc(alias = "CdlDemute")]
pub const CMD_DEMUTE: u8 = 0x0C;
/// `Setmode` command.
#[doc(alias = "Setmode")]
#[doc(alias = "CdlSetmode")]
pub const CMD_SETMODE: u8 = 0x0E;
/// `GetlocP` command: current physical play position.
#[doc(alias = "GetlocP")]
#[doc(alias = "CdlGetlocP")]
pub const CMD_GETLOCP: u8 = 0x11;
/// `SeekL` command: seek in data mode, homing on the data-sector headers.
#[doc(alias = "SeekL")]
#[doc(alias = "CdlSeekL")]
pub const CMD_SEEKL: u8 = 0x15;

/// Status byte: CD-DA playback in progress.
///
/// The drive sets at most one of the three activity bits
/// ([`STAT_PLAYING`], [`STAT_SEEKING`], [`STAT_READING`]) at a time;
/// between activities (stopping, spinning up before a seek engages) it
/// reports NONE of them for a second or two on real hardware, a window
/// emulators currently skip past.
pub const STAT_PLAYING: u8 = 0x80;
/// Status byte: a seek is in progress. See [`STAT_PLAYING`].
pub const STAT_SEEKING: u8 = 0x40;
/// Status byte: a sector read is in progress. See [`STAT_PLAYING`].
pub const STAT_READING: u8 = 0x20;
/// Status byte: the spindle motor is running. Independent of the activity
/// bits: the motor stays on between them.
pub const STAT_MOTOR_ON: u8 = 0x02;

/// CD-ROM XA (Mode 2) sector subheader bytes and the audio sector layout, from
/// nocash psx-spx "CDROM XA Subheader, File, Channel, Interleave" and
/// "CDROM XA Audio ADPCM Compression".
pub mod xa {
    /// Submode bit: last sector of a record (set with [`SUBMODE_EOF`]).
    pub const SUBMODE_EOR: u8 = 1 << 0;
    /// Submode bit: video sector.
    pub const SUBMODE_VIDEO: u8 = 1 << 1;
    /// Submode bit: audio sector (XA-ADPCM).
    pub const SUBMODE_AUDIO: u8 = 1 << 2;
    /// Submode bit: data sector.
    pub const SUBMODE_DATA: u8 = 1 << 3;
    /// Submode bit: Form 2 (2324 data bytes) rather than Form 1 (2048).
    pub const SUBMODE_FORM2: u8 = 1 << 5;
    /// Submode bit: real time, the drive may skip error correction.
    pub const SUBMODE_REAL_TIME: u8 = 1 << 6;
    /// Submode bit: last sector of the file.
    pub const SUBMODE_EOF: u8 = 1 << 7;

    /// Coding info bits 0..=1 value for a stereo sector (0 is mono).
    pub const CODING_STEREO: u8 = 1;
    /// Coding info bit 2: 18900 Hz rather than 37800 Hz.
    pub const CODING_RATE_18900: u8 = 1 << 2;

    /// Number of 128-byte sound groups in an audio sector.
    pub const GROUPS_PER_SECTOR: usize = 18;
    /// Size of one sound group: 16 header bytes and 28 data words.
    pub const GROUP_BYTES: usize = 128;
    /// Samples in one ADPCM block (one channel of one sound unit).
    pub const BLOCK_SAMPLES: usize = 28;
    /// Bytes of ADPCM in an audio sector.
    pub const SECTOR_ADPCM_BYTES: usize = GROUPS_PER_SECTOR * GROUP_BYTES;
    /// Data bytes of a Form 2 sector.
    pub const FORM2_DATA_BYTES: usize = 2324;
    /// Sectors per second the drive reads at normal speed (double speed reads twice as many).
    pub const SECTORS_PER_SECOND: u32 = 75;
    /// Filler sectors a song file ends with, so a drive that reads a little
    /// past the end meets audio sectors (which raise no data IRQ) and not
    /// whatever follows the file on the disc.
    pub const END_GUARD_SECTORS: u32 = 16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_file_is_four_consecutive_bytes() {
        assert_eq!(
            [
                reg::INDEX,
                reg::COMMAND_RESPONSE,
                reg::PARAMETER,
                reg::REQUEST_IRQ
            ],
            [0x1F80_1800, 0x1F80_1801, 0x1F80_1802, 0x1F80_1803]
        );
    }

    #[test]
    fn status_and_interrupt_codes_follow_psx_spx() {
        assert_eq!(index_status::PARAM_NOT_FULL, 0x10);
        assert_eq!(index_status::RESPONSE_NOT_EMPTY, 0x20);
        assert_eq!(index_status::DATA_FIFO_NOT_EMPTY, 0x40);
        assert_eq!(index_status::COMMAND_BUSY, 0x80);
        assert_eq!(
            [
                irq::DATA_READY,
                irq::COMPLETE,
                irq::ACKNOWLEDGE,
                irq::DATA_END,
                irq::ERROR
            ],
            [1, 2, 3, 4, 5]
        );
        assert_eq!(irq::ACK_ALL, 0x1F);
        assert_eq!(request::WANT_DATA, 0x80);
    }
}
