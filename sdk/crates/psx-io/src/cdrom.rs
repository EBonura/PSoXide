//! CD-ROM controller MMIO helpers.
//!
//! The controller exposes four byte registers selected by the low two
//! bits of the index register at [`BASE`]. These helpers cover the
//! command subset needed for CD-DA playback demos.

use crate::{irq, read8, write8};

/// CD-ROM register base.
pub const BASE: u32 = 0x1F80_1800;

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

/// CdlPlay command.
pub const CMD_PLAY: u8 = 0x03;
/// CdlSetloc command.
pub const CMD_SETLOC: u8 = 0x02;
/// CdlReadN command.
pub const CMD_READN: u8 = 0x06;
/// CdlGetStat command.
pub const CMD_GETSTAT: u8 = 0x01;
/// CdlStop command.
pub const CMD_STOP: u8 = 0x08;
/// CdlPause command.
pub const CMD_PAUSE: u8 = 0x09;
/// CdlMute command.
pub const CMD_MUTE: u8 = 0x0B;
/// CdlDemute command.
pub const CMD_DEMUTE: u8 = 0x0C;
/// CdlSetmode command.
pub const CMD_SETMODE: u8 = 0x0E;
/// CdlGetlocP command: current physical play position.
pub const CMD_GETLOCP: u8 = 0x11;
/// CdlSeekL command: seek in data mode, homing on the data-sector headers.
pub const CMD_SEEKL: u8 = 0x15;
/// CdlInit command: reset mode, abort pending reads, spin the motor up.
pub const CMD_INIT: u8 = 0x0A;

const REG_INDEX: u32 = BASE;
const REG_COMMAND_RESPONSE: u32 = BASE + 1;
const REG_PARAMETER: u32 = BASE + 2;
const REG_REQUEST_IRQ: u32 = BASE + 3;

const STATUS_PARAM_NOT_FULL: u8 = 1 << 4;
const STATUS_RESPONSE_NOT_EMPTY: u8 = 1 << 5;
const IRQ_ACK: u8 = 3;
const IRQ_DATA_READY: u8 = 1;
const IRQ_COMPLETE: u8 = 2;
const IRQ_ERROR: u8 = 5;
const IRQ_ACK_ALL: u8 = 0x1F;
const IRQ_PARAM_FIFO_RESET: u8 = 0x40;

/// Fixed-size command response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Response {
    bytes: [u8; 16],
    len: usize,
}

impl Response {
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

/// Send a command and return its first response packet.
pub fn command(command: u8, params: &[u8]) -> Response {
    let irq_enable = begin_polled_command();
    select_index(0);
    for &param in params {
        wait_param_room();
        write_byte(REG_PARAMETER, param);
    }
    write_byte(REG_COMMAND_RESPONSE, command);
    let irq = wait_irq(IRQ_ACK);
    finish_polled_command(irq_enable, irq)
}

/// Try to send a command and capture its first response packet.
///
/// Returns `None` if the controller does not expose parameter room or
/// a response within `spin_limit` polls. Use this for gameplay paths
/// where a not-ready drive should not stall rendering forever. If a
/// dispatched command times out, CD-ROM IRQ output remains masked so a
/// late ACK cannot interrupt a polling caller.
pub fn try_command(command: u8, params: &[u8], spin_limit: u32) -> Option<Response> {
    let irq_enable = begin_polled_command();
    select_index(0);
    for &param in params {
        if !wait_param_room_bounded(spin_limit) {
            restore_irq_enable(irq_enable);
            select_index(0);
            return None;
        }
        write_byte(REG_PARAMETER, param);
    }
    write_byte(REG_COMMAND_RESPONSE, command);
    let irq = wait_irq_bounded(IRQ_ACK, spin_limit)?;
    Some(finish_polled_command(irq_enable, irq))
}

/// Current CD-ROM IRQ flag value (0 = none, 1 = data ready, 2 = complete,
/// 3 = ack, 5 = error).
///
/// Exposed so callers can build their own wait loops bounded by a hardware
/// timer rather than by a poll count. A poll budget is only a proxy for time
/// and drifts with CPU and bus speed, which matters when the thing being
/// measured is mechanical.
pub fn irq_flag_value() -> u8 {
    irq_flag()
}

/// Acknowledge the given CD-ROM IRQ bits.
pub fn acknowledge_irq(bits: u8) {
    ack_irq(bits);
}

/// Drain any pending response bytes, discarding them.
pub fn discard_response() {
    drain_response_fifo();
}

/// Send a command without waiting for any response. Pairs with
/// [`irq_flag_value`] for caller-timed waits.
///
/// Returns `false` if the parameter FIFO never made room. Leaves CD-ROM IRQ
/// output masked exactly as the polled helpers do, so a late ACK cannot
/// interrupt the caller mid-measurement; call [`restore_irq_output`] when done.
pub fn dispatch_command(command: u8, params: &[u8], spin_limit: u32) -> Option<u8> {
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

/// Restore the CD-ROM IRQ enable saved by [`dispatch_command`].
pub fn restore_irq_output(saved: u8) {
    drain_response_fifo();
    ack_irq(IRQ_ACK_ALL);
    restore_irq_enable(saved);
    select_index(0);
}

/// Wait for the next streamed data sector (INT1) and acknowledge it.
///
/// Use between [`try_read_n`] and [`try_pause_until_complete`] to step through
/// a sector stream. Unrelated pending IRQs are drained and acknowledged so a
/// stale response cannot be mistaken for a sector arrival. Returns `false` on
/// a drive error or if no sector arrives within `spin_limit` polls.
pub fn try_wait_data_sector(spin_limit: u32) -> bool {
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
pub fn get_stat() -> Response {
    command(CMD_GETSTAT, &[])
}

/// Try to get the CD-ROM drive status byte.
pub fn try_get_stat(spin_limit: u32) -> Option<Response> {
    try_command(CMD_GETSTAT, &[], spin_limit)
}

/// Set the CD-ROM controller mode byte.
pub fn set_mode(mode: u8) -> Response {
    command(CMD_SETMODE, &[mode])
}

/// Try to set the CD-ROM controller mode byte.
pub fn try_set_mode(mode: u8, spin_limit: u32) -> Option<Response> {
    try_command(CMD_SETMODE, &[mode], spin_limit)
}

/// Seek to a logical data-sector LBA (the command receives absolute BCD MSF).
pub fn try_set_loc_lba(lba: u32, spin_limit: u32) -> Option<Response> {
    let [minute, second, frame] = lba_to_bcd_msf(lba);
    try_command(
        CMD_SETLOC,
        &[minute, second, frame],
        spin_limit,
    )
}

const fn lba_to_bcd_msf(lba: u32) -> [u8; 3] {
    let absolute = lba.saturating_add(150);
    let raw_minute = absolute / (60 * 75);
    let minute = if raw_minute > 99 { 99 } else { raw_minute as u8 };
    let second = ((absolute / 75) % 60) as u8;
    let frame = (absolute % 75) as u8;
    [bin_to_bcd(minute), bin_to_bcd(second), bin_to_bcd(frame)]
}

/// Begin a normal data-sector stream at the most recently selected location.
pub fn try_read_n(spin_limit: u32) -> Option<Response> {
    try_command(CMD_READN, &[], spin_limit)
}

/// Route CD-DA/XA output out of the CD-ROM controller.
pub fn demute() -> Response {
    command(CMD_DEMUTE, &[])
}

/// Try to route CD-DA/XA output out of the CD-ROM controller.
pub fn try_demute(spin_limit: u32) -> Option<Response> {
    try_command(CMD_DEMUTE, &[], spin_limit)
}

/// Mute CD-DA/XA output at the CD-ROM controller.
pub fn mute() -> Response {
    command(CMD_MUTE, &[])
}

/// Try to mute CD-DA/XA output at the CD-ROM controller.
pub fn try_mute(spin_limit: u32) -> Option<Response> {
    try_command(CMD_MUTE, &[], spin_limit)
}

/// Start CD-DA playback at a 1-based track number.
///
/// The number is relative to this program's own tracks; on a multi-program
/// disc [`crate::disc_base`] shifts it past whatever came before.
pub fn play_track(track: u8) -> Response {
    command(CMD_PLAY, &[bin_to_bcd(crate::disc_base::shift_track(track))])
}

/// Try to start CD-DA playback at a 1-based track number. Shifted like
/// [`play_track`].
pub fn try_play_track(track: u8, spin_limit: u32) -> Option<Response> {
    try_command(
        CMD_PLAY,
        &[bin_to_bcd(crate::disc_base::shift_track(track))],
        spin_limit,
    )
}

/// Pause CD-DA/read playback.
pub fn pause() -> Response {
    command(CMD_PAUSE, &[])
}

/// Try to pause CD-DA/read playback.
pub fn try_pause(spin_limit: u32) -> Option<Response> {
    try_command(CMD_PAUSE, &[], spin_limit)
}

/// Try to pause CD-DA/read playback and wait for the completion IRQ.
///
/// Unlike [`try_stop`], this leaves the drive spun up, which makes it the
/// right handoff before gameplay code starts issuing data-read commands.
pub fn try_pause_until_complete(spin_limit: u32) -> bool {
    try_command_until_complete_inner(CMD_PAUSE, &[], spin_limit)
}

/// Stop the CD-ROM motor/playback.
pub fn stop() -> Response {
    command(CMD_STOP, &[])
}

/// Try to stop the CD-ROM motor/playback.
pub fn try_stop(spin_limit: u32) -> Option<Response> {
    try_command(CMD_STOP, &[], spin_limit)
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

/// Get the current physical play position (CdlGetlocP). The 8-byte reply is
/// `[Track(bcd), Index(raw), RMM, RSS, RSECT, AMM, ASS, ASECT]`; parse it with
/// [`PlayPosition::parse`].
pub fn get_loc_p() -> Response {
    command(CMD_GETLOCP, &[])
}

/// Try to get the current physical play position, giving up after `spin_limit`.
pub fn try_get_loc_p(spin_limit: u32) -> Option<Response> {
    try_command(CMD_GETLOCP, &[], spin_limit)
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

fn wait_irq(expected: u8) -> u8 {
    loop {
        let irq = irq_flag();
        if irq == expected || irq == IRQ_ERROR {
            return irq;
        }
        if irq != 0 {
            let _ = read_response_fifo();
            ack_irq(irq);
        }
        core::hint::spin_loop();
    }
}

fn wait_irq_bounded(expected: u8, mut spins: u32) -> Option<u8> {
    loop {
        let irq = irq_flag();
        if irq == expected {
            return Some(irq);
        }
        if irq == IRQ_ERROR {
            return None;
        }
        if irq != 0 {
            let _ = read_response_fifo();
            ack_irq(irq);
        }
        if spins == 0 {
            return None;
        }
        spins -= 1;
        core::hint::spin_loop();
    }
}

/// Send a command and wait for its SECOND response (the completion IRQ),
/// not just the initial acknowledgement.
///
/// Seek, read and init all acknowledge immediately and finish much later, so
/// timing them against the ack measures command dispatch rather than the
/// mechanical operation. Returns `false` if either response fails to arrive
/// within `spin_limit` polls.
pub fn try_command_until_complete(command: u8, params: &[u8], spin_limit: u32) -> bool {
    try_command_until_complete_inner(command, params, spin_limit)
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

fn wait_param_room() {
    while read_status() & STATUS_PARAM_NOT_FULL == 0 {
        core::hint::spin_loop();
    }
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

fn clear_parameter_fifo() {
    select_index(1);
    write_byte(REG_REQUEST_IRQ, IRQ_PARAM_FIFO_RESET);
    select_index(0);
}

fn ack_irq(bits: u8) {
    select_index(1);
    write_byte(REG_REQUEST_IRQ, bits & IRQ_ACK_ALL);
    irq::ack(1 << irq::source::CDROM);
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
    unsafe { read8(addr) }
}

fn write_byte(addr: u32, value: u8) {
    // SAFETY: fixed CD-ROM MMIO register write.
    unsafe { write8(addr, value) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
