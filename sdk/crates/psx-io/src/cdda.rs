//! Renamed to [`crate::cd::audio`].

use crate::cd::audio;

/// Renamed to [`audio::PlaybackClock`].
#[deprecated(note = "renamed to `psx_io::cd::audio::PlaybackClock`")]
pub type CddaClock = audio::PlaybackClock;
/// Renamed to [`audio::EndDetector`].
#[deprecated(note = "renamed to `psx_io::cd::audio::EndDetector`")]
pub type CddaEndDetector = audio::EndDetector;
/// Renamed to [`audio::PlaybackStarter`].
#[deprecated(note = "renamed to `psx_io::cd::audio::PlaybackStarter`")]
pub type CddaStarter = audio::PlaybackStarter;

/// Moved to [`audio::COLD_DRIVE_DELAY_TICKS`].
#[deprecated(note = "moved to `psx_io::cd::audio::COLD_DRIVE_DELAY_TICKS`")]
pub const COLD_DRIVE_DELAY_TICKS: u32 = audio::COLD_DRIVE_DELAY_TICKS;
/// Moved to [`audio::DEFAULT_COMMAND_SPINS`].
#[deprecated(note = "moved to `psx_io::cd::audio::DEFAULT_COMMAND_SPINS`")]
pub const DEFAULT_COMMAND_SPINS: u32 = audio::DEFAULT_COMMAND_SPINS;
