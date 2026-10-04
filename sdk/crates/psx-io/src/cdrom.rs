//! Renamed to [`crate::cd`].
//!
//! Every item here forwards to its new name and is deprecated; the
//! register constants moved to [`psx_hw::cd`].

use crate::cd;
use crate::periph::Cd;

/// A token for a forwarder that never took one.
fn token() -> Cd {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `crate::periph`), and the old free functions never took one.
    unsafe { Cd::steal() }
}

/// Moved to [`cd::PlayPosition`].
#[deprecated(note = "moved to `psx_io::cd::PlayPosition`")]
pub type PlayPosition = cd::PlayPosition;
/// Moved to [`cd::SectorPollError`].
#[deprecated(note = "moved to `psx_io::cd::SectorPollError`")]
pub type SectorPollError = cd::SectorPollError;

/// Moved to [`cd::try_command`].
#[deprecated(note = "moved to `psx_io::cd::try_command`")]
#[inline(always)]
pub fn try_command(command: u8, params: &[u8], spin_limit: u32) -> Option<cd::Response> {
    token().try_command(command, params, spin_limit)
}

/// Moved to [`cd::irq_flag_value`].
#[deprecated(note = "moved to `psx_io::cd::irq_flag_value`")]
#[inline(always)]
pub fn irq_flag_value() -> u8 {
    token().irq_flag_value()
}

/// Moved to [`cd::acknowledge_irq`].
#[deprecated(note = "moved to `psx_io::cd::acknowledge_irq`")]
#[inline(always)]
pub fn acknowledge_irq(bits: u8) {
    token().acknowledge_irq(bits)
}

/// Moved to [`cd::discard_response`].
#[deprecated(note = "moved to `psx_io::cd::discard_response`")]
#[inline(always)]
pub fn discard_response() {
    token().discard_response()
}

/// Moved to [`cd::dispatch_command`].
#[deprecated(note = "moved to `psx_io::cd::dispatch_command`")]
#[inline(always)]
pub fn dispatch_command(command: u8, params: &[u8], spin_limit: u32) -> Option<u8> {
    token().dispatch_command(command, params, spin_limit)
}

/// Moved to [`cd::restore_irq_output`].
#[deprecated(note = "moved to `psx_io::cd::restore_irq_output`")]
#[inline(always)]
pub fn restore_irq_output(saved: u8) {
    token().restore_irq_output(saved)
}

/// Moved to [`cd::poll_data_sector`].
#[deprecated(note = "moved to `psx_io::cd::poll_data_sector`")]
#[inline(always)]
pub fn poll_data_sector() -> Result<bool, cd::SectorPollError> {
    token().poll_data_sector()
}

/// Renamed to [`cd::try_status`].
#[deprecated(note = "renamed to `psx_io::cd::try_status`")]
#[inline(always)]
pub fn try_get_stat(spin_limit: u32) -> Option<cd::Response> {
    token().try_status(spin_limit)
}

/// Moved to [`cd::try_set_mode`].
#[deprecated(note = "moved to `psx_io::cd::try_set_mode`")]
#[inline(always)]
pub fn try_set_mode(mode: u8, spin_limit: u32) -> Option<cd::Response> {
    token().try_set_mode(mode, spin_limit)
}

/// Renamed to [`cd::try_set_target_lba`].
#[deprecated(note = "renamed to `psx_io::cd::try_set_target_lba`")]
#[inline(always)]
pub fn try_set_loc_lba(lba: u32, spin_limit: u32) -> Option<cd::Response> {
    token().try_set_target_lba(lba, spin_limit)
}

/// Renamed to [`cd::try_unmute`].
#[deprecated(note = "renamed to `psx_io::cd::try_unmute`")]
#[inline(always)]
pub fn try_demute(spin_limit: u32) -> Option<cd::Response> {
    token().try_unmute(spin_limit)
}

/// Moved to [`crate::periph::Cd::try_pause`].
#[deprecated(note = "use `Cd::try_pause` with the `Cd` token")]
#[inline(always)]
pub fn try_pause(spin_limit: u32) -> Option<cd::Response> {
    token().try_pause(spin_limit)
}

/// Moved to [`cd::try_pause_until_complete`].
#[deprecated(note = "moved to `psx_io::cd::try_pause_until_complete`")]
#[inline(always)]
pub fn try_pause_until_complete(spin_limit: u32) -> bool {
    token().try_pause_until_complete(spin_limit)
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

/// Renamed to [`cd::try_play_position`].
#[deprecated(note = "renamed to `psx_io::cd::try_play_position`")]
#[inline(always)]
pub fn try_get_loc_p(spin_limit: u32) -> Option<cd::Response> {
    token().try_play_position(spin_limit)
}

/// Moved to [`crate::periph::Cd::set_audio_mixer`].
#[deprecated(note = "use `Cd::set_audio_mixer` with the `Cd` token")]
#[inline(always)]
pub fn set_audio_mixer(left_to_left: u8, left_to_right: u8, right_to_right: u8, right_to_left: u8) {
    token().set_audio_mixer(left_to_left, left_to_right, right_to_right, right_to_left)
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

/// Moved to [`psx_hw::cd::MODE_DOUBLE_SPEED`].
#[deprecated(note = "moved to `psx_hw::cd::MODE_DOUBLE_SPEED`")]
pub const MODE_DOUBLE_SPEED: u8 = psx_hw::cd::MODE_DOUBLE_SPEED;

/// Moved to [`psx_hw::cd::CMD_SETLOC`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SETLOC`")]
pub const CMD_SETLOC: u8 = psx_hw::cd::CMD_SETLOC;

/// Moved to [`psx_hw::cd::CMD_READN`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_READN`")]
pub const CMD_READN: u8 = psx_hw::cd::CMD_READN;

/// Moved to [`psx_hw::cd::CMD_GETSTAT`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_GETSTAT`")]
pub const CMD_GETSTAT: u8 = psx_hw::cd::CMD_GETSTAT;

/// Moved to [`psx_hw::cd::CMD_PAUSE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_PAUSE`")]
pub const CMD_PAUSE: u8 = psx_hw::cd::CMD_PAUSE;

/// Moved to [`psx_hw::cd::CMD_SETMODE`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SETMODE`")]
pub const CMD_SETMODE: u8 = psx_hw::cd::CMD_SETMODE;

/// Moved to [`psx_hw::cd::CMD_SEEKL`].
#[deprecated(note = "moved to `psx_hw::cd::CMD_SEEKL`")]
pub const CMD_SEEKL: u8 = psx_hw::cd::CMD_SEEKL;
