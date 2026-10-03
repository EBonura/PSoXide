//! CD-ROM controller registers, command bytes and status bits.
//!
//! Reference: nocash PSX-SPX "CDROM Controller I/O Ports" and "CDROM Controller
//! Command Summary".

/// Controller register base (index/status register; ports 1..3 follow).
pub const BASE: u32 = 0x1F80_1800;

/// Setmode bit: allow CD-DA playback via `Play`.
pub const MODE_CDDA: u8 = 1 << 0;
/// Setmode bit: auto-pause at the end of the track. With it set, the drive
/// pauses and raises INT4 at the end of the current track instead of playing
/// on into the next track / lead-out, so software can detect end-of-track and
/// loop without seeking the laser mid-playback.
pub const MODE_AUTO_PAUSE: u8 = 1 << 1;
/// Setmode bit: emit periodic play-report IRQs.
pub const MODE_REPORT: u8 = 1 << 2;
/// Setmode bit: double-speed data reads.
pub const MODE_DOUBLE_SPEED: u8 = 1 << 7;

/// `Getstat` command (PsyQ `CdlNop`).
#[doc(alias = "Getstat")]
#[doc(alias = "CdlNop")]
pub const CMD_GETSTAT: u8 = 0x01;
/// `Setloc` command.
#[doc(alias = "Setloc")]
#[doc(alias = "CdlSetloc")]
pub const CMD_SETLOC: u8 = 0x02;
/// `Play` command.
#[doc(alias = "Play")]
#[doc(alias = "CdlPlay")]
pub const CMD_PLAY: u8 = 0x03;
/// `ReadN` command.
#[doc(alias = "ReadN")]
#[doc(alias = "CdlReadN")]
pub const CMD_READN: u8 = 0x06;
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

/// Status byte: CD-DA playback is in progress. The drive reports at most one
/// of the playing / seeking / reading bits at a time, and while it settles
/// after a command it can report none of them for a second or two on real
/// hardware.
pub const STAT_PLAYING: u8 = 0x80;
/// Status byte: a seek is in progress. See [`STAT_PLAYING`].
pub const STAT_SEEKING: u8 = 0x40;
/// Status byte: a sector read is in progress. See [`STAT_PLAYING`].
pub const STAT_READING: u8 = 0x20;
/// Status byte: the spindle motor is running. Independent of the activity
/// bits: the motor stays on between them.
pub const STAT_MOTOR_ON: u8 = 0x02;
