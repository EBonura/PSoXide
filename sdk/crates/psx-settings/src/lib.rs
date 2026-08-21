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

use psx_pad::ActionMap;

/// On-card record magic.
pub const MAGIC: [u8; 4] = *b"PSST";
/// Current record format version.
pub const VERSION: u8 = 1;
/// Header bytes before action bindings and scores.
const HEADER_LEN: usize = 16;
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
    /// Display brightness, 0..=100.
    pub brightness: u8,
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
            brightness: 75,
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

    /// Exact encoded byte length for this profile shape.
    pub const fn encoded_len() -> usize {
        HEADER_LEN + ACTIONS * BINDING_LEN + SCORES * SCORE_LEN
    }

    /// Clamp user-controlled values to safe shared ranges.
    pub fn sanitize(&mut self) {
        self.brightness = self.brightness.min(100);
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
        out[8] = self.brightness;
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
        out[15] = checksum(&out[..15], &out[HEADER_LEN..len]);
        Ok(len)
    }

    /// Decode a profile of this exact action/score shape.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let len = Self::encoded_len();
        if len > MAX_RECORD_LEN || bytes.len() < len {
            return Err(CodecError::BufferTooSmall);
        }
        if bytes[..4] != MAGIC {
            return Err(CodecError::BadMagic);
        }
        if bytes[4] != VERSION {
            return Err(CodecError::UnsupportedVersion);
        }
        if bytes[5] as usize != ACTIONS || bytes[6] as usize != SCORES {
            return Err(CodecError::WrongShape);
        }
        if bytes[15] != checksum(&bytes[..15], &bytes[HEADER_LEN..len]) {
            return Err(CodecError::BadChecksum);
        }
        let mut cursor = HEADER_LEN;
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
            brightness: bytes[8],
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
#[cfg(feature = "card")]
pub fn save<B: psx_mc::Block, const ACTIONS: usize, const SCORES: usize>(
    card: &mut psx_mc::Card<B>,
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    let mut bytes = [0u8; MAX_RECORD_LEN];
    let len = profile.encode(&mut bytes).map_err(CardError::Codec)?;
    card.write(name, title, &bytes[..len])
        .map_err(CardError::Card)
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

/// Save a profile to the controller-1 memory-card slot, formatting a blank
/// card first. Existing files with the same name are overwritten.
#[cfg(feature = "card")]
pub fn save_slot_one<const ACTIONS: usize, const SCORES: usize>(
    name: &str,
    title: &str,
    profile: &Profile<ACTIONS, SCORES>,
) -> Result<(), CardError> {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::new(psx_mc::Slot::One));
    match card.is_formatted().map_err(CardError::Card)? {
        true => {}
        false => card.format().map_err(CardError::Card)?,
    }
    save(&mut card, name, title, profile)
}

/// Load a profile from the controller-1 memory-card slot.
#[cfg(feature = "card")]
pub fn load_slot_one<const ACTIONS: usize, const SCORES: usize>(
    name: &str,
) -> Result<Profile<ACTIONS, SCORES>, CardError> {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::new(psx_mc::Slot::One));
    load(&mut card, name)
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

    #[test]
    fn profile_round_trips_and_sanitizes() {
        let mut original = Profile::<2, 3>::new(ACTIONS);
        original.brightness = 140;
        original.look_speed_percent = 220;
        original.set_invert_y(true);
        original.high_scores = [7, 42, 9];
        let mut bytes = [0u8; MAX_RECORD_LEN];
        let len = original.encode(&mut bytes).unwrap();
        let decoded = Profile::<2, 3>::decode(&bytes[..len]).unwrap();
        assert_eq!(decoded.brightness, 100);
        assert_eq!(decoded.look_speed_percent, 200);
        assert!(decoded.invert_y());
        assert_eq!(decoded.actions, ACTIONS);
        assert_eq!(decoded.high_scores, [7, 42, 9]);
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
}
