// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared, allocator-free game preferences.
//!
//! The record intentionally contains only settings that recur across PSoXide
//! games. Games remain free to keep simulation or renderer-specific options in
//! their own save data. The byte format is explicit and checksummed so a newer
//! executable can reject a torn or incompatible card write and fall back to
//! its shipped defaults.

#![no_std]
#![warn(missing_docs)]

use psx_display::{Brightness, ScreenOffset};
use psx_pad::ActionMap;

/// On-card record magic.
pub const MAGIC: [u8; 4] = *b"PSST";
/// Current record format version.
///
/// Version 1 held a 0..=100 brightness percentage that no game used. Version 2
/// replaces it with the [`Brightness`] step (the save byte of
/// [`Brightness::to_byte`]) and adds the [`ScreenOffset`] bytes after the
/// checksum. [`Profile::decode`] still reads version 1 records and gives the
/// two picture options their defaults; [`Profile::encode`] writes version 2.
pub const VERSION: u8 = 2;
/// The oldest record version [`Profile::decode`] reads.
const OLDEST_VERSION: u8 = 1;
/// Header bytes before action bindings and scores, in a version 1 record.
const HEADER_LEN_V1: usize = 16;
/// Header bytes before action bindings and scores, in a version 2 record:
/// the version 1 header plus the screen X and Y bytes.
const HEADER_LEN: usize = 18;
/// Bytes occupied by one action binding.
const BINDING_LEN: usize = 4;
/// Bytes occupied by one high score.
const SCORE_LEN: usize = 4;
/// Largest settings record accepted by the card helpers.
pub const MAX_RECORD_LEN: usize = 128;

/// Settings flag: invert vertical look.
pub const FLAG_INVERT_Y: u8 = 1 << 0;

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
    /// Bit flags such as [`FLAG_INVERT_Y`].
    pub flags: u8,
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
            actions,
            high_scores: [0; SCORES],
        }
    }

    /// Exact encoded byte length for this profile shape (the current version).
    pub const fn encoded_len() -> usize {
        HEADER_LEN + ACTIONS * BINDING_LEN + SCORES * SCORE_LEN
    }

    /// Clamp user-controlled values to safe shared ranges.
    pub fn sanitize(&mut self) {
        // `Brightness` and `ScreenOffset` clamp in their constructors, so any
        // value a profile holds is already inside -5..=5 and -16..=16.
        self.sfx_volume = self.sfx_volume.min(100);
        self.music_volume = self.music_volume.min(100);
        self.move_deadzone = self.move_deadzone.clamp(0, 64);
        self.look_deadzone = self.look_deadzone.clamp(0, 64);
        self.look_speed_percent = self.look_speed_percent.clamp(25, 200);
        self.flags &= FLAG_INVERT_Y;
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

    /// Encode into `out`, returning the bytes used.
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
        let [screen_x, screen_y] = self.screen_offset.to_bytes();
        out[16] = screen_x;
        out[17] = screen_y;
        out[15] = checksum(&out[..15], &out[HEADER_LEN..len]);
        Ok(len)
    }

    /// Decode a profile of this exact action/score shape, from a record of
    /// the current version or of version 1 (which carries no picture options,
    /// so those read as their defaults).
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
        let header_len = if version == 1 {
            HEADER_LEN_V1
        } else {
            HEADER_LEN
        };
        let len = header_len + ACTIONS * BINDING_LEN + SCORES * SCORE_LEN;
        if len > MAX_RECORD_LEN || bytes.len() < len {
            return Err(CodecError::BufferTooSmall);
        }
        if bytes[5] as usize != ACTIONS || bytes[6] as usize != SCORES {
            return Err(CodecError::WrongShape);
        }
        if bytes[15] != checksum(&bytes[..15], &bytes[header_len..len]) {
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
            bytes[15] = checksum(&bytes[..15], &bytes[HEADER_LEN..len]);
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
    fn a_version_one_record_is_rewritten_as_version_two() {
        let (old, old_len) = version_one_record(75);
        let mut loaded = Profile::<2, 3>::decode(&old[..old_len]).unwrap();
        loaded.brightness = loaded.brightness.stepped(2);
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = loaded.encode(&mut bytes).unwrap();
        assert_eq!(len, old_len + 2);
        assert_eq!(bytes[4], 2);
        let reloaded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(reloaded, loaded);
        assert_eq!(reloaded.brightness.level(), 2);
        assert_eq!(reloaded.high_scores, [7, 42, 9]);
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
    fn a_version_one_card_file_loads_and_is_replaced_by_version_two() {
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
