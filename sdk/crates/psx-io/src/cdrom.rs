//! Renamed to [`crate::cd`].
//!
//! Every item here forwards to its new name and is deprecated; the
//! register constants moved to [`psx_hw::cd`].

use crate::cd;

/// Moved to [`cd::Response`].
#[deprecated(note = "moved to `psx_io::cd::Response`")]
pub type Response = cd::Response;
/// Moved to [`cd::PlayPosition`].
#[deprecated(note = "moved to `psx_io::cd::PlayPosition`")]
pub type PlayPosition = cd::PlayPosition;
/// Moved to [`cd::SectorPollError`].
#[deprecated(note = "moved to `psx_io::cd::SectorPollError`")]
pub type SectorPollError = cd::SectorPollError;

/// Moved to [`cd::command`].
#[deprecated(note = "moved to `psx_io::cd::command`")]
#[inline(always)]
pub fn command(command: u8, params: &[u8]) -> cd::Response {
    cd::command(command, params)
}

/// Moved to [`cd::try_command`].
#[deprecated(note = "moved to `psx_io::cd::try_command`")]
#[inline(always)]
pub fn try_command(command: u8, params: &[u8], spin_limit: u32) -> Option<cd::Response> {
    cd::try_command(command, params, spin_limit)
}

/// Moved to [`cd::irq_flag_value`].
#[deprecated(note = "moved to `psx_io::cd::irq_flag_value`")]
#[inline(always)]
pub fn irq_flag_value() -> u8 {
    cd::irq_flag_value()
}

/// Moved to [`cd::acknowledge_irq`].
#[deprecated(note = "moved to `psx_io::cd::acknowledge_irq`")]
#[inline(always)]
pub fn acknowledge_irq(bits: u8) {
    cd::acknowledge_irq(bits)
}

/// Moved to [`cd::discard_response`].
#[deprecated(note = "moved to `psx_io::cd::discard_response`")]
#[inline(always)]
pub fn discard_response() {
    cd::discard_response()
}

/// Moved to [`cd::dispatch_command`].
#[deprecated(note = "moved to `psx_io::cd::dispatch_command`")]
#[inline(always)]
pub fn dispatch_command(command: u8, params: &[u8], spin_limit: u32) -> Option<u8> {
    cd::dispatch_command(command, params, spin_limit)
}

/// Moved to [`cd::restore_irq_output`].
#[deprecated(note = "moved to `psx_io::cd::restore_irq_output`")]
#[inline(always)]
pub fn restore_irq_output(saved: u8) {
    cd::restore_irq_output(saved)
}

/// Moved to [`cd::poll_data_sector`].
#[deprecated(note = "moved to `psx_io::cd::poll_data_sector`")]
#[inline(always)]
pub fn poll_data_sector() -> Result<bool, cd::SectorPollError> {
    cd::poll_data_sector()
}

/// Moved to [`cd::try_wait_data_sector`].
#[deprecated(note = "moved to `psx_io::cd::try_wait_data_sector`")]
#[inline(always)]
pub fn try_wait_data_sector(spin_limit: u32) -> bool {
    cd::try_wait_data_sector(spin_limit)
}

/// Renamed to [`cd::status`].
#[deprecated(note = "renamed to `psx_io::cd::status`")]
#[inline(always)]
pub fn get_stat() -> cd::Response {
    cd::status()
}

/// Renamed to [`cd::try_status`].
#[deprecated(note = "renamed to `psx_io::cd::try_status`")]
#[inline(always)]
pub fn try_get_stat(spin_limit: u32) -> Option<cd::Response> {
    cd::try_status(spin_limit)
}

/// Moved to [`cd::set_mode`].
#[deprecated(note = "moved to `psx_io::cd::set_mode`")]
#[inline(always)]
pub fn set_mode(mode: u8) -> cd::Response {
    cd::set_mode(mode)
}

/// Moved to [`cd::try_set_mode`].
#[deprecated(note = "moved to `psx_io::cd::try_set_mode`")]
#[inline(always)]
pub fn try_set_mode(mode: u8, spin_limit: u32) -> Option<cd::Response> {
    cd::try_set_mode(mode, spin_limit)
}

/// Renamed to [`cd::try_set_target_lba`].
#[deprecated(note = "renamed to `psx_io::cd::try_set_target_lba`")]
#[inline(always)]
pub fn try_set_loc_lba(lba: u32, spin_limit: u32) -> Option<cd::Response> {
    cd::try_set_target_lba(lba, spin_limit)
}

/// Renamed to [`cd::try_start_reading`].
#[deprecated(note = "renamed to `psx_io::cd::try_start_reading`")]
#[inline(always)]
pub fn try_read_n(spin_limit: u32) -> Option<cd::Response> {
    cd::try_start_reading(spin_limit)
}

/// Renamed to [`cd::unmute`].
#[deprecated(note = "renamed to `psx_io::cd::unmute`")]
#[inline(always)]
pub fn demute() -> cd::Response {
    cd::unmute()
}

/// Renamed to [`cd::try_unmute`].
#[deprecated(note = "renamed to `psx_io::cd::try_unmute`")]
#[inline(always)]
pub fn try_demute(spin_limit: u32) -> Option<cd::Response> {
    cd::try_unmute(spin_limit)
}

/// Moved to [`cd::mute`].
#[deprecated(note = "moved to `psx_io::cd::mute`")]
#[inline(always)]
pub fn mute() -> cd::Response {
    cd::mute()
}

/// Moved to [`cd::try_mute`].
#[deprecated(note = "moved to `psx_io::cd::try_mute`")]
#[inline(always)]
pub fn try_mute(spin_limit: u32) -> Option<cd::Response> {
    cd::try_mute(spin_limit)
}

/// Moved to [`cd::play_track`].
#[deprecated(note = "moved to `psx_io::cd::play_track`")]
#[inline(always)]
pub fn play_track(track: u8) -> cd::Response {
    cd::play_track(track)
}

/// Moved to [`cd::try_play_track`].
#[deprecated(note = "moved to `psx_io::cd::try_play_track`")]
#[inline(always)]
pub fn try_play_track(track: u8, spin_limit: u32) -> Option<cd::Response> {
    cd::try_play_track(track, spin_limit)
}

/// Moved to [`cd::pause`].
#[deprecated(note = "moved to `psx_io::cd::pause`")]
#[inline(always)]
pub fn pause() -> cd::Response {
    cd::pause()
}

/// Moved to [`cd::try_pause`].
#[deprecated(note = "moved to `psx_io::cd::try_pause`")]
#[inline(always)]
pub fn try_pause(spin_limit: u32) -> Option<cd::Response> {
    cd::try_pause(spin_limit)
}

/// Moved to [`cd::try_pause_until_complete`].
#[deprecated(note = "moved to `psx_io::cd::try_pause_until_complete`")]
#[inline(always)]
pub fn try_pause_until_complete(spin_limit: u32) -> bool {
    cd::try_pause_until_complete(spin_limit)
}

/// Moved to [`cd::stop`].
#[deprecated(note = "moved to `psx_io::cd::stop`")]
#[inline(always)]
pub fn stop() -> cd::Response {
    cd::stop()
}

/// Moved to [`cd::try_stop`].
#[deprecated(note = "moved to `psx_io::cd::try_stop`")]
#[inline(always)]
pub fn try_stop(spin_limit: u32) -> Option<cd::Response> {
    cd::try_stop(spin_limit)
}

/// Moved to [`cd::stop_and_settle`].
#[deprecated(note = "moved to `psx_io::cd::stop_and_settle`")]
#[inline(always)]
pub fn stop_and_settle(spin_limit: u32, max_polls: u32) -> bool {
    cd::stop_and_settle(spin_limit, max_polls)
}

/// Moved to [`cd::bin_to_bcd`].
#[deprecated(note = "moved to `psx_io::cd::bin_to_bcd`")]
#[inline(always)]
pub const fn bin_to_bcd(v: u8) -> u8 {
    cd::bin_to_bcd(v)
}

/// Moved to [`cd::bcd_to_bin`].
#[deprecated(note = "moved to `psx_io::cd::bcd_to_bin`")]
#[inline(always)]
pub const fn bcd_to_bin(v: u8) -> u8 {
    cd::bcd_to_bin(v)
}

/// Renamed to [`cd::play_position`].
#[deprecated(note = "renamed to `psx_io::cd::play_position`")]
#[inline(always)]
pub fn get_loc_p() -> cd::Response {
    cd::play_position()
}

/// Renamed to [`cd::try_play_position`].
#[deprecated(note = "renamed to `psx_io::cd::try_play_position`")]
#[inline(always)]
pub fn try_get_loc_p(spin_limit: u32) -> Option<cd::Response> {
    cd::try_play_position(spin_limit)
}

/// Moved to [`cd::try_command_until_complete`].
#[deprecated(note = "moved to `psx_io::cd::try_command_until_complete`")]
#[inline(always)]
pub fn try_command_until_complete(command: u8, params: &[u8], spin_limit: u32) -> bool {
    cd::try_command_until_complete(command, params, spin_limit)
}

/// Moved to [`cd::set_audio_mixer`].
#[deprecated(note = "moved to `psx_io::cd::set_audio_mixer`")]
#[inline(always)]
pub fn set_audio_mixer(left_to_left: u8, left_to_right: u8, right_to_right: u8, right_to_left: u8) {
    cd::set_audio_mixer(left_to_left, left_to_right, right_to_right, right_to_left)
}

/// Moved to [`psx_hw::cd::BASE`].
#[deprecated(note = "moved to `psx_hw::cd::BASE`")]
pub const BASE: u32 = psx_hw::cd::BASE;

/// Moved to [`psx_hw::cd::MODE_CDDA`].
#[deprecated(note = "moved to `psx_hw::cd::MODE_CDDA`")]
pub const MODE_CDDA: u8 = psx_hw::cd::MODE_CDDA;

/// Moved to [`psx_hw::cd::MODE_AUTO_PAUSE`].
#[deprecated(note = "moved to `psx_hw::cd::MODE_AUTO_PAUSE`")]
pub const MODE_AUTO_PAUSE: u8 = psx_hw::cd::MODE_AUTO_PAUSE;

/// Moved to [`psx_hw::cd::MODE_REPORT`].
#[deprecated(note = "moved to `psx_hw::cd::MODE_REPORT`")]
pub const MODE_REPORT: u8 = psx_hw::cd::MODE_REPORT;

/// Moved to [`psx_hw::cd::MODE_DOUBLE_SPEED`].
#[deprecated(note = "moved to `psx_hw::cd::MODE_DOUBLE_SPEED`")]
pub const MODE_DOUBLE_SPEED: u8 = psx_hw::cd::MODE_DOUBLE_SPEED;

/// Moved to [`psx_hw::cd::CMD_PLAY`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_PLAY`")]
pub const CMD_PLAY: u8 = psx_hw::cd::CMD_PLAY;

/// Moved to [`psx_hw::cd::CMD_SETLOC`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SETLOC`")]
pub const CMD_SETLOC: u8 = psx_hw::cd::CMD_SETLOC;

/// Moved to [`psx_hw::cd::CMD_READN`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_READN`")]
pub const CMD_READN: u8 = psx_hw::cd::CMD_READN;

/// Moved to [`psx_hw::cd::CMD_GETSTAT`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_GETSTAT`")]
pub const CMD_GETSTAT: u8 = psx_hw::cd::CMD_GETSTAT;

/// Moved to [`psx_hw::cd::CMD_STOP`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_STOP`")]
pub const CMD_STOP: u8 = psx_hw::cd::CMD_STOP;

/// Moved to [`psx_hw::cd::CMD_PAUSE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_PAUSE`")]
pub const CMD_PAUSE: u8 = psx_hw::cd::CMD_PAUSE;

/// Moved to [`psx_hw::cd::CMD_MUTE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_MUTE`")]
pub const CMD_MUTE: u8 = psx_hw::cd::CMD_MUTE;

/// Moved to [`psx_hw::cd::CMD_DEMUTE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_DEMUTE`")]
pub const CMD_DEMUTE: u8 = psx_hw::cd::CMD_DEMUTE;

/// Moved to [`psx_hw::cd::CMD_SETMODE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SETMODE`")]
pub const CMD_SETMODE: u8 = psx_hw::cd::CMD_SETMODE;

/// Moved to [`psx_hw::cd::CMD_GETLOCP`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_GETLOCP`")]
pub const CMD_GETLOCP: u8 = psx_hw::cd::CMD_GETLOCP;

/// Moved to [`psx_hw::cd::CMD_SEEKL`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SEEKL`")]
pub const CMD_SEEKL: u8 = psx_hw::cd::CMD_SEEKL;

/// Moved to [`psx_hw::cd::CMD_INIT`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_INIT`")]
pub const CMD_INIT: u8 = psx_hw::cd::CMD_INIT;

/// Moved to [`psx_hw::cd::STAT_PLAYING`].
#[deprecated(note = "moved to `psx_hw::cd::STAT_PLAYING`")]
pub const STAT_PLAYING: u8 = psx_hw::cd::STAT_PLAYING;

/// Moved to [`psx_hw::cd::STAT_SEEKING`].
#[deprecated(note = "moved to `psx_hw::cd::STAT_SEEKING`")]
pub const STAT_SEEKING: u8 = psx_hw::cd::STAT_SEEKING;

/// Moved to [`psx_hw::cd::STAT_READING`].
#[deprecated(note = "moved to `psx_hw::cd::STAT_READING`")]
pub const STAT_READING: u8 = psx_hw::cd::STAT_READING;

/// Moved to [`psx_hw::cd::STAT_MOTOR_ON`].
#[deprecated(note = "moved to `psx_hw::cd::STAT_MOTOR_ON`")]
pub const STAT_MOTOR_ON: u8 = psx_hw::cd::STAT_MOTOR_ON;
