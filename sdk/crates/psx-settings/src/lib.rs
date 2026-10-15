// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared, allocator-free game preferences.
//!
//! The record holds settings that recur across PSoXide games, seven
//! game-owned flag bits and [`GAME_BYTES_LEN`] game-owned bytes. Games remain
//! free to keep larger simulation or renderer-specific data in their own saves.
//! The byte format is explicit and checksummed so a newer executable can reject
//! a torn or incompatible card write and fall back to its shipped defaults.

#![no_std]
#![warn(missing_docs)]

use psx_display::{Brightness, ScreenOffset};
use psx_pad::ActionMap;

/// On-card record magic.
pub const MAGIC: [u8; 4] = *b"PSST";
/// Current record format version.
///
/// Version 1 held a 0..=100 brightness percentage that no game used. Version 2
/// replaced it with the [`Brightness`] step (the save byte of
/// [`Brightness::to_byte`]) and added the [`ScreenOffset`] bytes after the
/// checksum. Version 3 adds [`GAME_BYTES_LEN`] game-owned bytes after the
/// screen offset and brings those two offset bytes under the checksum.
/// [`Profile::decode`] reads versions 1 and 2 and gives every field they lack
/// its default (the picture options for version 1, zeroed game bytes for both);
/// [`Profile::encode`] writes version 3.
///
/// An executable built before version 3 refuses a version 3 record with
/// [`CodecError::UnsupportedVersion`] and falls back to its shipped defaults,
/// exactly as it does for a torn record. A record carries no way for older code
/// to skip a layout it does not know, so there is no partial load: keep a game
/// and its save on the same SDK, or accept a settings reset on rollback.
pub const VERSION: u8 = 3;
/// The oldest record version [`Profile::decode`] reads.
const OLDEST_VERSION: u8 = 1;
/// Header bytes before action bindings and scores, in a version 1 record.
const HEADER_LEN_V1: usize = 16;
/// Header bytes before action bindings and scores, in a version 2 record:
/// the version 1 header plus the screen X and Y bytes.
const HEADER_LEN_V2: usize = 18;
/// Number of game-defined bytes in a [`Profile`] (version 3 onward).
///
/// Eight bytes keep every record far inside [`MAX_RECORD_LEN`] (the largest
/// shipping shape, 9 actions and no scores, grows from 54 to 62 bytes) and a
/// settings file is one 8 KiB memory-card block whatever its length, so the
/// card footprint does not change.
pub const GAME_BYTES_LEN: usize = 8;
/// Header bytes before action bindings and scores, in a version 3 record:
/// the version 2 header plus [`GAME_BYTES_LEN`] game bytes.
const HEADER_LEN: usize = HEADER_LEN_V2 + GAME_BYTES_LEN;
/// Offset of the checksum byte in every version.
const CHECKSUM_AT: usize = 15;
/// Bytes occupied by one action binding.
const BINDING_LEN: usize = 4;
/// Bytes occupied by one high score.
const SCORE_LEN: usize = 4;
/// Largest settings record accepted by the card helpers.
pub const MAX_RECORD_LEN: usize = 128;

/// Settings flag: invert vertical look.
pub const FLAG_INVERT_Y: u8 = 1 << 0;
/// Bits in the existing settings flag byte owned by each game. Bit 0 stays
/// reserved for [`FLAG_INVERT_Y`], and no future shared setting may use these
/// seven bits. Unknown game bits survive loading, sanitizing and saving with
/// this SDK. Older executables masked these bits on load and may erase them on
/// a later save, so rolling back a game build cannot preserve their meaning.
pub const GAME_FLAGS_MASK: u8 = !FLAG_INVERT_Y;

/// A game profile shared by menus, input and memory-card persistence.
///
/// `ACTIONS` is the number of game-defined logical actions. `SCORES` is the
/// number of high-score slots (usually one per difficulty).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Profile<const ACTIONS: usize, const SCORES: usize> {
    /// Picture brightness: `DEFAULT`, `DARKER 1..=5` or `BRIGHTER 1..=5`.
    pub brightness: Brightness,
    /// Picture position on the television, within 16 pixels each way.
    pub screen_offset: ScreenOffset,
    /// Sound-effect volume, 0..=100.
    pub sfx_volume: u8,
    /// Music volume, 0..=100.
    pub music_volume: u8,
    /// Left-stick deadzone in raw stick counts.
    pub move_deadzone: u8,
    /// Right-stick deadzone in raw stick counts.
    pub look_deadzone: u8,
    /// Look-speed percentage.
    pub look_speed_percent: u8,
    /// Game-selected difficulty index.
    pub difficulty: u8,
    /// Bit flags: bit 0 is [`FLAG_INVERT_Y`], bits 1 through 7 belong to the
    /// game. Prefer [`Self::set_game_flag`] so unrelated bits survive edits.
    pub flags: u8,
    /// Bytes reserved for the game's own settings, zero by default and stored
    /// verbatim (version 3 onward). The SDK never reads, clamps or interprets
    /// them, so a game must range-check what it loads from its own slots.
    /// Name each slot with a `const` index in the game, for example
    /// `const FOV_BYTE: usize = 0;`. A slot a game has not written reads as 0,
    /// including from a version 1 or 2 record, so make 0 mean "default".
    pub game_bytes: [u8; GAME_BYTES_LEN],
    /// Logical action bindings.
    pub actions: ActionMap<ACTIONS>,
    /// Persistent high-score slots.
    pub high_scores: [u32; SCORES],
}

impl<const ACTIONS: usize, const SCORES: usize> Profile<ACTIONS, SCORES> {
    /// Construct a profile with common presentation/input defaults and the
    /// caller's game-specific action map.
    pub const fn new(actions: ActionMap<ACTIONS>) -> Self {
        Self {
            brightness: Brightness::DEFAULT,
            screen_offset: ScreenOffset::CENTRE,
            sfx_volume: 100,
            music_volume: 100,
            move_deadzone: 18,
            look_deadzone: 12,
            look_speed_percent: 100,
            difficulty: 1,
            flags: 0,
            game_bytes: [0; GAME_BYTES_LEN],
            actions,
            high_scores: [0; SCORES],
        }
    }

    /// Exact encoded byte length for this profile shape (the current version).
    pub const fn encoded_len() -> usize {
        HEADER_LEN + ACTIONS * BINDING_LEN + SCORES * SCORE_LEN
    }

    /// Clamp user-controlled values to safe shared ranges. Flag bits are
    /// preserved because the game owns bits 1 through 7.
    pub fn sanitize(&mut self) {
        // `Brightness` and `ScreenOffset` clamp in their constructors, so any
        // value a profile holds is already inside -5..=5 and -16..=16.
        self.sfx_volume = self.sfx_volume.min(100);
        self.music_volume = self.music_volume.min(100);
        self.move_deadzone = self.move_deadzone.clamp(0, 64);
        self.look_deadzone = self.look_deadzone.clamp(0, 64);
        self.look_speed_percent = self.look_speed_percent.clamp(25, 200);
    }

    /// Whether vertical look is inverted.
    pub const fn invert_y(&self) -> bool {
        self.flags & FLAG_INVERT_Y != 0
    }

    /// Enable or disable inverted vertical look.
    pub fn set_invert_y(&mut self, enabled: bool) {
        if enabled {
            self.flags |= FLAG_INVERT_Y;
        } else {
            self.flags &= !FLAG_INVERT_Y;
        }
    }

    /// Return the raw game-owned bits in the settings flag byte.
    pub const fn game_flags(&self) -> u8 {
        self.flags & GAME_FLAGS_MASK
    }

    /// Set or clear the selected game-owned bits without changing any other
    /// bits. Bit 0 is ignored even if it appears in `mask`.
    pub fn set_game_flag(&mut self, mask: u8, enabled: bool) {
        let mask = mask & GAME_FLAGS_MASK;
        if enabled {
            self.flags |= mask;
        } else {
            self.flags &= !mask;
        }
    }

    /// Raise one high-score slot when `score` beats it. Returns `true` if the
    /// profile changed and should be persisted.
    pub fn submit_score(&mut self, slot: usize, score: u32) -> bool {
        let Some(best) = self.high_scores.get_mut(slot) else {
            return false;
        };
        if score <= *best {
            return false;
        }
        *best = score;
        true
    }

    /// Encode into `out` as a version 3 record, returning the bytes used.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CodecError> {
        let len = Self::encoded_len();
        if len > MAX_RECORD_LEN || out.len() < len {
            return Err(CodecError::BufferTooSmall);
        }
        out[..len].fill(0);
        out[..4].copy_from_slice(&MAGIC);
        out[4] = VERSION;
        out[5] = ACTIONS as u8;
        out[6] = SCORES as u8;
        out[7] = self.flags;
        out[8] = self.brightness.to_byte();
        out[9] = self.sfx_volume;
        out[10] = self.music_volume;
        out[11] = self.move_deadzone;
        out[12] = self.look_deadzone;
        out[13] = self.look_speed_percent;
        out[14] = self.difficulty;
        let [screen_x, screen_y] = self.screen_offset.to_bytes();
        out[16] = screen_x;
        out[17] = screen_y;
        out[HEADER_LEN_V2..HEADER_LEN].copy_from_slice(&self.game_bytes);
        let mut cursor = HEADER_LEN;
        for binding in self.actions.bindings() {
            put_u16(out, cursor, binding.primary);
            put_u16(out, cursor + 2, binding.secondary);
            cursor += BINDING_LEN;
        }
        for score in self.high_scores {
            put_u32(out, cursor, score);
            cursor += SCORE_LEN;
        }
        // Version 3 seals every byte but the checksum itself.
        out[CHECKSUM_AT] = checksum(&out[..CHECKSUM_AT], &out[CHECKSUM_AT + 1..len]);
        Ok(len)
    }

    /// Decode a profile of this exact action/score shape, from a record of
    /// the current version or of version 1 or 2. A version 1 record carries no
    /// picture options, so those read as their defaults; a record older than
    /// version 3 carries no game bytes, so those read as zero. Every other
    /// field is preserved.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.len() < HEADER_LEN_V1 {
            return Err(CodecError::BufferTooSmall);
        }
        if bytes[..4] != MAGIC {
            return Err(CodecError::BadMagic);
        }
        let version = bytes[4];
        if !(OLDEST_VERSION..=VERSION).contains(&version) {
            return Err(CodecError::UnsupportedVersion);
        }
        let header_len = match version {
            1 => HEADER_LEN_V1,
            2 => HEADER_LEN_V2,
            _ => HEADER_LEN,
        };
        let len = header_len + ACTIONS * BINDING_LEN + SCORES * SCORE_LEN;
        if len > MAX_RECORD_LEN || bytes.len() < len {
            return Err(CodecError::BufferTooSmall);
        }
        if bytes[5] as usize != ACTIONS || bytes[6] as usize != SCORES {
            return Err(CodecError::WrongShape);
        }
        // Versions 1 and 2 sealed the first 15 bytes and the payload after
        // the header, leaving the version 2 screen offset bytes unprotected;
        // version 3 seals everything but the checksum.
        let sealed = if version >= 3 {
            checksum(&bytes[..CHECKSUM_AT], &bytes[CHECKSUM_AT + 1..len])
        } else {
            checksum(&bytes[..CHECKSUM_AT], &bytes[header_len..len])
        };
        if bytes[CHECKSUM_AT] != sealed {
            return Err(CodecError::BadChecksum);
        }
        let (brightness, screen_offset) = if version == 1 {
            (Brightness::DEFAULT, ScreenOffset::CENTRE)
        } else {
            (
                Brightness::from_byte(bytes[8]),
                ScreenOffset::from_bytes([bytes[16], bytes[17]]),
            )
        };
        let mut game_bytes = [0; GAME_BYTES_LEN];
        if version >= 3 {
            game_bytes.copy_from_slice(&bytes[HEADER_LEN_V2..HEADER_LEN]);
        }
        let mut cursor = header_len;
        let mut bindings = [psx_pad::ActionBinding::UNBOUND; ACTIONS];
        for binding in &mut bindings {
            binding.primary = get_u16(bytes, cursor);
            binding.secondary = get_u16(bytes, cursor + 2);
            cursor += BINDING_LEN;
        }
        let mut high_scores = [0; SCORES];
        for score in &mut high_scores {
            *score = get_u32(bytes, cursor);
            cursor += SCORE_LEN;
        }
        let mut profile = Self {
            brightness,
            screen_offset,
            sfx_volume: bytes[9],
            music_volume: bytes[10],
            move_deadzone: bytes[11],
            look_deadzone: bytes[12],
            look_speed_percent: bytes[13],
            difficulty: bytes[14],
            flags: bytes[7],
            game_bytes,
            actions: ActionMap::new(bindings),
            high_scores,
        };
        profile.sanitize();
        Ok(profile)
    }
}

/// Settings codec failure.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// The supplied byte slice cannot contain this profile shape.
    BufferTooSmall,
    /// The record is not a PSoXide settings record.
    BadMagic,
    /// The record belongs to an unsupported format version.
    UnsupportedVersion,
    /// The record carries different action or score counts.
    WrongShape,
    /// The record was damaged or only partly written.
    BadChecksum,
}

/// Persistence failure from the settings layer.
#[cfg(feature = "card")]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CardError {
    /// Memory-card filesystem or transport failure.
    Card(psx_mc::Error),
    /// Settings codec failure.
    Codec(CodecError),
}

/// Save one profile into a standard PS1 memory-card file.
///
/// A card without the `MC` header is not written to: this returns
/// `CardError::Card(psx_mc::Error::NotFormatted)` and leaves the card as it
/// was, so the game can ask the player before calling
/// [`format_and_save`]. Formatting drops every save on the card from its
/// directory.
#[cfg(feature = "card")]
pub fn save<B: psx_mc::Block, const ACTIONS: usize, const SCORES: usize>(
    card: &mut psx_mc::Card<B>,
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    if !card.is_formatted().map_err(CardError::Card)? {
        return Err(CardError::Card(psx_mc::Error::NotFormatted));
    }
    let mut bytes = [0u8; MAX_RECORD_LEN];
    let len = profile.encode(&mut bytes).map_err(CardError::Codec)?;
    card.write(name, title, &bytes[..len])
        .map_err(CardError::Card)
}

/// Format the card if it has no `MC` header, then save the profile. Call
/// this only after the player agreed to format: it replaces the directory of
/// an unformatted card, and every save the card held stops being listed. A
/// formatted card is left alone and the profile is saved onto it.
#[cfg(feature = "card")]
pub fn format_and_save<B: psx_mc::Block, const ACTIONS: usize, const SCORES: usize>(
    card: &mut psx_mc::Card<B>,
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    if !card.is_formatted().map_err(CardError::Card)? {
        card.format().map_err(CardError::Card)?;
    }
    save(card, name, title, profile)
}

/// Load one exact profile shape from a standard PS1 memory-card file.
#[cfg(feature = "card")]
pub fn load<B: psx_mc::Block, const ACTIONS: usize, const SCORES: usize>(
    card: &mut psx_mc::Card<B>,
    name: &str,
) -> Result<Profile<ACTIONS, SCORES>, CardError> {
    let mut bytes = [0u8; MAX_RECORD_LEN];
    let len = card.read(name, &mut bytes).map_err(CardError::Card)?;
    Profile::decode(&bytes[..len]).map_err(CardError::Codec)
}

/// Save a profile to the controller-1 memory-card slot, driving the port
/// through `port`. Existing files with the same name are replaced. A card that
/// is not formatted is refused with `CardError::Card(psx_mc::Error::NotFormatted)`
/// and not touched; use [`format_and_save_to_slot_one`] once the player has
/// agreed to format.
#[cfg(feature = "card")]
pub fn save_to_slot_one<const ACTIONS: usize, const SCORES: usize>(
    port: &mut psx_mc::ControllerPort,
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(port, psx_mc::Slot::One));
    save(&mut card, name, title, profile)
}

/// [`save_to_slot_one`] on a token the caller does not hold.
#[cfg(feature = "card")]
#[deprecated(note = "use `save_to_slot_one` with the `ControllerPort` token")]
pub fn save_slot_one<const ACTIONS: usize, const SCORES: usize>(
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    save_to_slot_one(&mut steal_port(), name, title, profile)
}

/// [`format_and_save`] on the controller-1 memory-card slot. This is the
/// call that can erase a card's directory; reach it only from an explicit
/// "format this card?" answer.
#[cfg(feature = "card")]
pub fn format_and_save_to_slot_one<const ACTIONS: usize, const SCORES: usize>(
    port: &mut psx_mc::ControllerPort,
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(port, psx_mc::Slot::One));
    format_and_save(&mut card, name, title, profile)
}

/// Load a profile from the controller-1 memory-card slot, driving the port
/// through `port`.
#[cfg(feature = "card")]
pub fn load_from_slot_one<const ACTIONS: usize, const SCORES: usize>(
    port: &mut psx_mc::ControllerPort,
    name: &str,
) -> Result<Profile<ACTIONS, SCORES>, CardError> {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(port, psx_mc::Slot::One));
    load(&mut card, name)
}

/// [`load_from_slot_one`] on a token the caller does not hold.
#[cfg(feature = "card")]
#[deprecated(note = "use `load_from_slot_one` with the `ControllerPort` token")]
pub fn load_slot_one<const ACTIONS: usize, const SCORES: usize>(
    name: &str,
) -> Result<Profile<ACTIONS, SCORES>, CardError> {
    load_from_slot_one(&mut steal_port(), name)
}

/// The token for a deprecated forwarder that never took one.
#[cfg(feature = "card")]
fn steal_port() -> psx_mc::ControllerPort {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `psx_io::periph`), and the old functions never took one.
    unsafe { psx_mc::ControllerPort::steal() }
}

#[inline]
fn put_u16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[inline]
fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[inline]
fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn checksum(header: &[u8], payload: &[u8]) -> u8 {
    let mut value = 0xA7u8;
    for byte in header.iter().chain(payload) {
        value = value.rotate_left(1) ^ *byte;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_pad::{button, ActionBinding};

    const ACTIONS: ActionMap<2> = ActionMap::new([
        ActionBinding::new(button::CROSS, button::CIRCLE),
        ActionBinding::new(button::START, 0),
    ]);

    /// A version 1 record exactly as the first release wrote it: a 16 byte
    /// header whose byte 8 is a 0..=100 brightness percentage.
    fn version_one_record(percent: u8) -> ([u8; MAX_RECORD_LEN], usize) {
        let mut out = [0u8; MAX_RECORD_LEN];
        let len = HEADER_LEN_V1 + 2 * BINDING_LEN + 3 * SCORE_LEN;
        out[..4].copy_from_slice(&MAGIC);
        out[4] = 1;
        out[5] = 2;
        out[6] = 3;
        out[7] = FLAG_INVERT_Y;
        out[8] = percent;
        out[9] = 80;
        out[10] = 60;
        out[11] = 20;
        out[12] = 14;
        out[13] = 120;
        out[14] = 2;
        put_u16(&mut out, 16, button::CROSS);
        put_u16(&mut out, 18, button::CIRCLE);
        put_u16(&mut out, 20, button::START);
        put_u16(&mut out, 22, 0);
        put_u32(&mut out, 24, 7);
        put_u32(&mut out, 28, 42);
        put_u32(&mut out, 32, 9);
        out[15] = checksum(&out[..15], &out[HEADER_LEN_V1..len]);
        (out, len)
    }

    /// A version 2 record exactly as the previous release wrote it: an 18
    /// byte header, no game bytes, and a checksum that leaves the two screen
    /// offset bytes unsealed.
    fn version_two_record(flags: u8) -> ([u8; MAX_RECORD_LEN], usize) {
        let mut out = [0u8; MAX_RECORD_LEN];
        let len = HEADER_LEN_V2 + 2 * BINDING_LEN + 3 * SCORE_LEN;
        out[..4].copy_from_slice(&MAGIC);
        out[4] = 2;
        out[5] = 2;
        out[6] = 3;
        out[7] = flags;
        out[8] = Brightness::new(-3).to_byte();
        out[9] = 80;
        out[10] = 60;
        out[11] = 20;
        out[12] = 14;
        out[13] = 120;
        // Version 2 shipped games stored their own value here.
        out[14] = 3;
        let [x, y] = ScreenOffset::<16>::new(5, -7).to_bytes();
        out[16] = x;
        out[17] = y;
        put_u16(&mut out, 18, button::CROSS);
        put_u16(&mut out, 20, button::CIRCLE);
        put_u16(&mut out, 22, button::START);
        put_u16(&mut out, 24, 0);
        put_u32(&mut out, 26, 7);
        put_u32(&mut out, 30, 42);
        put_u32(&mut out, 34, 9);
        out[15] = checksum(&out[..15], &out[HEADER_LEN_V2..len]);
        (out, len)
    }

    #[test]
    fn profile_round_trips_and_sanitizes() {
        let mut original = Profile::<2, 3>::new(ACTIONS);
        original.look_speed_percent = 220;
        original.set_invert_y(true);
        original.high_scores = [7, 42, 9];
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = original.encode(&mut bytes).unwrap();
        assert_eq!(len, Profile::<2, 3>::encoded_len());
        assert_eq!(bytes[4], VERSION);
        let decoded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(decoded.look_speed_percent, 200);
        assert!(decoded.invert_y());
        assert_eq!(decoded.actions, ACTIONS);
        assert_eq!(decoded.high_scores, [7, 42, 9]);
        assert_eq!(decoded.brightness, Brightness::DEFAULT);
        assert_eq!(decoded.screen_offset, ScreenOffset::CENTRE);
    }

    #[test]
    fn every_brightness_step_and_screen_offset_round_trips() {
        for level in -5..=5i8 {
            let mut original = Profile::<2, 1>::new(ACTIONS);
            original.brightness = Brightness::new(level);
            original.screen_offset = ScreenOffset::new(level * 3, -level * 3);
            let mut bytes = [0u8; MAX_RECORD_LEN];
            let len = original.encode(&mut bytes).unwrap();
            let decoded = Profile::<2, 1>::decode(&bytes[..len]).unwrap();
            assert_eq!(decoded.brightness.level(), level);
            assert_eq!(
                decoded.screen_offset.pixels(),
                ((level * 3) as i16, (-level * 3) as i16)
            );
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn the_brightness_byte_is_the_display_crates_save_byte() {
        let mut profile = Profile::<2, 1>::new(ACTIONS);
        profile.brightness = Brightness::new(-3);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        profile.encode(&mut bytes).unwrap();
        assert_eq!(bytes[8], Brightness::new(-3).to_byte());
        assert_eq!(bytes[8], 0xFD);
    }

    #[test]
    fn an_out_of_range_brightness_or_offset_byte_is_clamped_on_load() {
        let mut profile = Profile::<2, 1>::new(ACTIONS);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        // 0x7F is +127 and 0x80 is -128 in two's complement; 0x64 stands for
        // the old percentage scale. Re-seal the checksum after each edit.
        for (byte, level) in [(0x7F, 5), (0x80, -5), (0x64, 5), (0xFB, -5), (0x05, 5)] {
            bytes[8] = byte;
            bytes[16] = 0x7F;
            bytes[17] = 0x80;
            bytes[15] = checksum(&bytes[..15], &bytes[16..len]);
            profile = Profile::<2, 1>::decode(&bytes[..len]).unwrap();
            assert_eq!(profile.brightness.level(), level);
            assert_eq!(profile.screen_offset.pixels(), (16, -16));
        }
    }

    #[test]
    fn a_version_one_record_loads_with_the_picture_defaults() {
        // 75 was the shipped percentage; 100 is its ceiling; 0 its floor.
        for percent in [75, 100, 0] {
            let (bytes, len) = version_one_record(percent);
            let loaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
            assert_eq!(loaded.brightness, Brightness::DEFAULT);
            assert_eq!(loaded.screen_offset, ScreenOffset::CENTRE);
            // Everything else the old record held survives.
            assert_eq!(loaded.sfx_volume, 80);
            assert_eq!(loaded.music_volume, 60);
            assert_eq!(loaded.move_deadzone, 20);
            assert_eq!(loaded.look_deadzone, 14);
            assert_eq!(loaded.look_speed_percent, 120);
            assert_eq!(loaded.difficulty, 2);
            assert!(loaded.invert_y());
            assert_eq!(loaded.actions, ACTIONS);
            assert_eq!(loaded.high_scores, [7, 42, 9]);
        }
    }

    #[test]
    fn a_version_one_record_is_rewritten_as_the_current_version() {
        let (old, old_len) = version_one_record(75);
        let mut loaded = Profile::<2, 3>::decode(&old[..old_len]).unwrap();
        loaded.brightness = loaded.brightness.stepped(2);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = loaded.encode(&mut bytes).unwrap();
        assert_eq!(len, old_len + 2 + GAME_BYTES_LEN);
        assert_eq!(bytes[4], VERSION);
        let reloaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(reloaded, loaded);
        assert_eq!(reloaded.brightness.level(), 2);
        assert_eq!(reloaded.high_scores, [7, 42, 9]);
    }

    #[test]
    fn game_bits_survive_every_record_version_and_sanitize() {
        let (mut old, old_len) = version_one_record(75);
        old[7] = 0xA7;
        old[15] = checksum(&old[..15], &old[HEADER_LEN_V1..old_len]);
        let mut profile = Profile::<2, 3>::decode(&old[..old_len]).unwrap();
        assert_eq!(profile.flags, 0xA7);
        assert_eq!(profile.game_flags(), 0xA6);
        assert!(profile.invert_y());
        profile.sanitize();
        assert_eq!(profile.flags, 0xA7);

        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        assert_eq!(VERSION, 3);
        assert_eq!(len, old_len + 2 + GAME_BYTES_LEN);
        assert_eq!(bytes[7], 0xA7);
        assert_eq!(Profile::<2, 3>::decode(&bytes[..len]).unwrap(), profile);

        let (mut v2, v2_len) = version_two_record(0xA7);
        let loaded = Profile::<2, 3>::decode(&v2[..v2_len]).unwrap();
        assert_eq!(loaded.game_flags(), 0xA6);
        v2[7] ^= 0x40;
        assert_eq!(
            Profile::<2, 3>::decode(&v2[..v2_len]),
            Err(CodecError::BadChecksum)
        );
    }

    #[test]
    fn game_flag_edits_preserve_other_bits_and_cannot_change_invert_y() {
        let mut profile = Profile::<2, 1>::new(ACTIONS);
        profile.flags = 0xA7;
        profile.set_game_flag(0x02, false);
        assert_eq!(profile.flags, 0xA5);
        profile.set_game_flag(0x03, true);
        assert_eq!(profile.flags, 0xA7);
        profile.set_game_flag(FLAG_INVERT_Y, false);
        assert_eq!(profile.flags, 0xA7);
        profile.set_invert_y(false);
        assert_eq!(profile.flags, 0xA6);
        profile.set_game_flag(0x02, false);
        assert_eq!(profile.flags, 0xA4);
        profile.set_invert_y(true);
        assert_eq!(profile.flags, 0xA5);
    }

    #[test]
    fn game_bits_keep_the_current_length_and_checksum() {
        let mut profile = Profile::<5, 0>::new(ActionMap::new([ActionBinding::UNBOUND; 5]));
        profile.set_game_flag(0x80, true);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        assert_eq!(len, 18 + GAME_BYTES_LEN + 5 * BINDING_LEN);
        assert_eq!(len, 46);
        assert_eq!(bytes[4], 3);
        assert_eq!(bytes[7], 0x80);
        bytes[7] ^= 0x40;
        assert_eq!(
            Profile::<5, 0>::decode(&bytes[..len]),
            Err(CodecError::BadChecksum)
        );
    }

    #[test]
    fn a_torn_version_one_record_and_an_unknown_version_are_rejected() {
        let (mut bytes, len) = version_one_record(75);
        bytes[len - 1] ^= 0x01;
        assert_eq!(
            Profile::<2, 3>::decode(&bytes[..len]),
            Err(CodecError::BadChecksum)
        );
        let (mut bytes, len) = version_one_record(75);
        bytes[4] = 0;
        assert_eq!(
            Profile::<2, 3>::decode(&bytes[..len]),
            Err(CodecError::UnsupportedVersion)
        );
        bytes[4] = VERSION + 1;
        assert_eq!(
            Profile::<2, 3>::decode(&bytes[..len]),
            Err(CodecError::UnsupportedVersion)
        );
        // A version 1 record cut short of its payload is too small, not a
        // version 2 reading of the next bytes.
        let (bytes, len) = version_one_record(75);
        assert_eq!(
            Profile::<2, 3>::decode(&bytes[..len - 1]),
            Err(CodecError::BufferTooSmall)
        );
        // Another shape is refused for both versions.
        let (bytes, len) = version_one_record(75);
        assert_eq!(
            Profile::<2, 2>::decode(&bytes[..len]),
            Err(CodecError::WrongShape)
        );
    }

    #[cfg(feature = "card")]
    #[test]
    fn a_version_one_card_file_loads_and_is_replaced_by_the_current_version() {
        let (old, old_len) = version_one_record(75);
        let mut card = psx_mc::Card::new(psx_mc::RamCard::new());
        card.format().unwrap();
        card.write("BESLES-00000SETTEST1", "SETTINGS TEST", &old[..old_len])
            .unwrap();
        let mut profile = load::<_, 2, 3>(&mut card, "BESLES-00000SETTEST1").unwrap();
        assert_eq!(profile.brightness, Brightness::DEFAULT);
        profile.brightness = Brightness::new(-4);
        save(&mut card, "BESLES-00000SETTEST1", "SETTINGS TEST", &profile).unwrap();
        let reloaded = load::<_, 2, 3>(&mut card, "BESLES-00000SETTEST1").unwrap();
        assert_eq!(reloaded, profile);
    }

    #[test]
    fn game_bytes_default_to_zero_and_round_trip_verbatim() {
        let mut profile = Profile::<2, 3>::new(ACTIONS);
        assert_eq!(profile.game_bytes, [0; GAME_BYTES_LEN]);
        profile.game_bytes = [0, 255, 1, 128, 7, 0, 9, 200];
        profile.difficulty = 2;
        profile.high_scores = [7, 42, 9];
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        assert_eq!(&bytes[18..26], &[0, 255, 1, 128, 7, 0, 9, 200]);
        let loaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        // sanitize() must not touch them, and nothing else moves.
        assert_eq!(loaded, profile);
        assert_eq!(loaded.difficulty, 2);
    }

    #[test]
    fn a_version_two_record_loads_with_zero_game_bytes_and_every_field_kept() {
        let (bytes, len) = version_two_record(FLAG_INVERT_Y | 0x40);
        let loaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(loaded.game_bytes, [0; GAME_BYTES_LEN]);
        assert_eq!(loaded.brightness.level(), -3);
        assert_eq!(loaded.screen_offset.pixels(), (5, -7));
        assert_eq!(loaded.sfx_volume, 80);
        assert_eq!(loaded.music_volume, 60);
        assert_eq!(loaded.move_deadzone, 20);
        assert_eq!(loaded.look_deadzone, 14);
        assert_eq!(loaded.look_speed_percent, 120);
        assert_eq!(loaded.difficulty, 3);
        assert_eq!(loaded.flags, FLAG_INVERT_Y | 0x40);
        assert_eq!(loaded.actions, ACTIONS);
        assert_eq!(loaded.high_scores, [7, 42, 9]);
    }

    #[test]
    fn a_version_one_record_also_loads_with_zero_game_bytes() {
        let (bytes, len) = version_one_record(75);
        let loaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(loaded.game_bytes, [0; GAME_BYTES_LEN]);
    }

    #[test]
    fn a_version_two_record_is_rewritten_as_version_three_keeping_its_fields() {
        let (old, old_len) = version_two_record(FLAG_INVERT_Y);
        let mut loaded = Profile::<2, 3>::decode(&old[..old_len]).unwrap();
        loaded.game_bytes[3] = 41;
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = loaded.encode(&mut bytes).unwrap();
        assert_eq!(len, old_len + GAME_BYTES_LEN);
        assert_eq!(bytes[4], 3);
        let reloaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(reloaded, loaded);
        assert_eq!(reloaded.game_bytes[3], 41);
        assert_eq!(reloaded.screen_offset.pixels(), (5, -7));
        assert_eq!(reloaded.difficulty, 3);
        assert_eq!(reloaded.high_scores, [7, 42, 9]);
    }

    #[test]
    fn the_version_three_checksum_seals_the_game_bytes_and_screen_offset() {
        let mut profile = Profile::<2, 3>::new(ACTIONS);
        profile.game_bytes[0] = 5;
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        // Every byte but the checksum itself is covered: flipping any one
        // is caught.
        for at in (0..len).filter(|at| *at != 15 && *at != 4) {
            let mut torn = bytes;
            torn[at] ^= 0x01;
            let result = Profile::<2, 3>::decode(&torn[..len]);
            let expected = match at {
                0..=3 => Err(CodecError::BadMagic),
                5 | 6 => Err(CodecError::WrongShape),
                _ => Err(CodecError::BadChecksum),
            };
            assert_eq!(result, expected, "byte {at}");
        }
    }

    #[test]
    fn a_version_four_record_and_a_cut_short_version_three_record_are_rejected() {
        let profile = Profile::<2, 3>::new(ACTIONS);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        // A reader never guesses the layout of a version it does not know.
        let mut future = bytes;
        future[4] = VERSION + 1;
        assert_eq!(
            Profile::<2, 3>::decode(&future[..len]),
            Err(CodecError::UnsupportedVersion)
        );
        assert_eq!(
            Profile::<2, 3>::decode(&bytes[..len - 1]),
            Err(CodecError::BufferTooSmall)
        );
        // Another shape is refused.
        assert_eq!(
            Profile::<2, 2>::decode(&bytes[..len]),
            Err(CodecError::WrongShape)
        );
    }

    #[test]
    fn the_record_size_limit_is_checked_for_the_version_three_header() {
        // 18 + 8 + 4 * (actions + scores) must stay within MAX_RECORD_LEN.
        let fits = Profile::<20, 5>::new(ActionMap::new([ActionBinding::UNBOUND; 20]));
        assert_eq!(Profile::<20, 5>::encoded_len(), 126);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = fits.encode(&mut bytes).unwrap();
        assert_eq!(Profile::<20, 5>::decode(&bytes[..len]).unwrap(), fits);

        let too_big = Profile::<20, 6>::new(ActionMap::new([ActionBinding::UNBOUND; 20]));
        assert_eq!(Profile::<20, 6>::encoded_len(), 130);
        assert_eq!(too_big.encode(&mut bytes), Err(CodecError::BufferTooSmall));
        // A caller buffer shorter than the record is refused too.
        let small = Profile::<2, 1>::new(ACTIONS);
        let mut short = [0u8; 20];
        assert_eq!(small.encode(&mut short), Err(CodecError::BufferTooSmall));
    }

    #[test]
    fn every_shipping_shape_still_fits_in_one_card_block() {
        // NitroXide 9/0, VoXide 5/0 and the arcade microgames 0/3.
        assert_eq!(Profile::<9, 0>::encoded_len(), 62);
        assert_eq!(Profile::<5, 0>::encoded_len(), 46);
        assert_eq!(Profile::<0, 3>::encoded_len(), 38);
        assert!(Profile::<9, 0>::encoded_len() <= MAX_RECORD_LEN);
    }

    #[cfg(feature = "card")]
    #[test]
    fn a_version_two_card_file_loads_and_is_replaced_by_version_three() {
        let (old, old_len) = version_two_record(0);
        let mut card = psx_mc::Card::new(psx_mc::RamCard::new());
        card.format().unwrap();
        card.write("BESLES-00000SETTEST1", "SETTINGS TEST", &old[..old_len])
            .unwrap();
        let mut profile = load::<_, 2, 3>(&mut card, "BESLES-00000SETTEST1").unwrap();
        assert_eq!(profile.game_bytes, [0; GAME_BYTES_LEN]);
        profile.game_bytes[1] = 9;
        save(&mut card, "BESLES-00000SETTEST1", "SETTINGS TEST", &profile).unwrap();
        let reloaded = load::<_, 2, 3>(&mut card, "BESLES-00000SETTEST1").unwrap();
        assert_eq!(reloaded, profile);
        assert_eq!(reloaded.game_bytes[1], 9);
    }

    #[test]
    fn checksum_rejects_a_torn_record() {
        let profile = Profile::<2, 1>::new(ACTIONS);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = profile.encode(&mut bytes).unwrap();
        bytes[len - 1] ^= 0x80;
        assert_eq!(
            Profile::<2, 1>::decode(&bytes[..len]),
            Err(CodecError::BadChecksum)
        );
    }

    #[cfg(feature = "card")]
    #[test]
    fn memory_card_round_trip_uses_normal_filesystem() {
        let mut card = psx_mc::Card::new(psx_mc::RamCard::new());
        card.format().unwrap();
        let mut profile = Profile::<2, 2>::new(ACTIONS);
        profile.high_scores = [1234, 5678];
        save(&mut card, "BESLES-00000SETTEST1", "SETTINGS TEST", &profile).unwrap();
        let loaded = load::<_, 2, 2>(&mut card, "BESLES-00000SETTEST1").unwrap();
        assert_eq!(loaded, profile);
    }

    /// A card holding another game's save whose `MC` header frame was lost.
    #[cfg(feature = "card")]
    fn card_without_header() -> psx_mc::Card<psx_mc::RamCard> {
        let mut card = psx_mc::Card::new(psx_mc::RamCard::new());
        card.format().unwrap();
        card.write("BESLES-00000OTHER001", "OTHER", b"another save")
            .unwrap();
        let mut image = *card.into_inner().image();
        image[..psx_mc::FRAME_SIZE].fill(0);
        psx_mc::Card::new(psx_mc::RamCard::from_image(&image).unwrap())
    }

    #[cfg(feature = "card")]
    #[test]
    fn saving_to_an_unformatted_card_is_refused_and_touches_nothing() {
        let profile = Profile::<2, 1>::new(ACTIONS);
        let mut card = card_without_header();
        let before = *card.device().image();
        assert_eq!(
            save(&mut card, "BESLES-00000SETTEST1", "SETTINGS TEST", &profile),
            Err(CardError::Card(psx_mc::Error::NotFormatted))
        );
        assert_eq!(card.device().image(), &before);
    }

    #[cfg(feature = "card")]
    #[test]
    fn formatting_is_a_separate_explicit_call() {
        let profile = Profile::<2, 1>::new(ACTIONS);
        // A blank card is formatted and written.
        let mut blank = psx_mc::Card::new(psx_mc::RamCard::new());
        format_and_save(
            &mut blank,
            "BESLES-00000SETTEST1",
            "SETTINGS TEST",
            &profile,
        )
        .unwrap();
        assert_eq!(
            load::<_, 2, 1>(&mut blank, "BESLES-00000SETTEST1").unwrap(),
            profile
        );
        // A formatted card keeps its other saves.
        let mut card = psx_mc::Card::new(psx_mc::RamCard::new());
        card.format().unwrap();
        card.write("BESLES-00000OTHER001", "OTHER", b"another save")
            .unwrap();
        format_and_save(&mut card, "BESLES-00000SETTEST1", "SETTINGS TEST", &profile).unwrap();
        let mut buf = [0u8; 32];
        let n = card.read("BESLES-00000OTHER001", &mut buf).unwrap();
        assert_eq!(&buf[..n], b"another save");
    }
}
