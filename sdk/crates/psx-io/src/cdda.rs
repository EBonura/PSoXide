//! Renamed to [`crate::cd::audio`].

use crate::cd::audio;

/// Renamed to [`audio::EndDetector`].
#[deprecated(note = "renamed to `psx_io::cd::audio::EndDetector`")]
pub type CddaEndDetector = audio::EndDetector;
/// Renamed to [`audio::PlaybackStarter`].
#[deprecated(note = "renamed to `psx_io::cd::audio::PlaybackStarter`")]
pub type CddaStarter = audio::PlaybackStarter;
