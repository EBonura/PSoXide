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
/// Setmode bit: emit periodic play-report IRQs.
pub const MODE_REPORT: u8 = 1 << 2;
/// Setmode bit: double-speed data reads.
pub const MODE_DOUBLE_SPEED: u8 = 1 << 7;

/// CdlPlay command.
pub const CMD_PLAY: u8 = 0x03;
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

const REG_INDEX: u32 = BASE;
const REG_COMMAND_RESPONSE: u32 = BASE + 1;
const REG_PARAMETER: u32 = BASE + 2;
const REG_REQUEST_IRQ: u32 = BASE + 3;

const STATUS_PARAM_NOT_FULL: u8 = 1 << 4;
const STATUS_RESPONSE_NOT_EMPTY: u8 = 1 << 5;
const IRQ_ACK: u8 = 3;
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

/// Set the CD-ROM controller mode byte.
pub fn set_mode(mode: u8) -> Response {
    command(CMD_SETMODE, &[mode])
}

/// Try to set the CD-ROM controller mode byte.
pub fn try_set_mode(mode: u8, spin_limit: u32) -> Option<Response> {
    try_command(CMD_SETMODE, &[mode], spin_limit)
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
pub fn play_track(track: u8) -> Response {
    command(CMD_PLAY, &[bin_to_bcd(track)])
}

/// Try to start CD-DA playback at a 1-based track number.
pub fn try_play_track(track: u8, spin_limit: u32) -> Option<Response> {
    try_command(CMD_PLAY, &[bin_to_bcd(track)], spin_limit)
}

/// Pause CD-DA/read playback.
pub fn pause() -> Response {
    command(CMD_PAUSE, &[])
}

/// Try to pause CD-DA/read playback.
pub fn try_pause(spin_limit: u32) -> Option<Response> {
    try_command(CMD_PAUSE, &[], spin_limit)
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
}
