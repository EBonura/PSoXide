//! Sequenced music for the PS1 SPU.
//!
//! `psx-seq` plays songs written in the bytecode that Nintendo 64 games such
//! as Ocarina of Time use for their music: one *sequence* script starts up to
//! sixteen *channel* scripts, each channel starts up to four *layer* scripts,
//! and the layers hold the notes. Scripts branch, loop, call, read and write
//! IO ports the game also reads and writes, and so can follow the player:
//! mute a channel, change the tune with the time of day, fade a part in with
//! distance. That interactivity is why the bytecode runs at play time instead
//! of being flattened to a list of notes.
//!
//! The engine is `no_std`, has no heap, no floating point and no 64-bit
//! arithmetic. Sound goes out through [`psx_sfx::hw::Hw`], the same seam the
//! sound-effect mixer uses, so on a host a `Recorder` captures every register
//! write in order.
//!
//! # Time
//!
//! The engine advances in *updates* at [`UPDATE_HZ`] (180 per second). The
//! caller owns the clock: [`Sequencer::pump`] takes the number of updates
//! that have elapsed on whatever clock it has (a timer interrupt, the
//! HBlank counter polled from a wait loop) and runs the ones that are due.
//! After a long stall it runs at most [`MAX_CATCH_UP`] and drops the rest, so
//! a slow frame sounds like a gap in the music, not a stampede of notes.
//!
//! # Fixed point
//!
//! Gains are Q15 (32768 = 1.0), pitch multipliers Q16 (65536 = 1.0), the
//! envelope Q24. The original squares most volume terms (note velocity,
//! channel volume, envelope level); that curve is kept because it is what
//! shapes the dynamics.
//!
//! # Example
//!
//! A one-channel song (a hand-assembled sequence: `initchan`, `ldchan`,
//! `tempo`, `vol`, then a channel that picks instrument 0, plays a note on
//! layer 0 and ends) over an empty bank plays no sound but runs without error:
//!
//! ```
//! use psx_seq::Sequencer;
//! use psx_sfx::hw::Recorder;
//!
//! let mut hw = Recorder::<64>::new();
//! let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
//! seq.update(&mut hw);
//! assert!(hw.events().is_empty());
//! ```

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

mod engine;
pub mod format;
mod live;
mod note;
mod script;
pub mod tables;
mod types;

pub use live::NoteHandle;
pub use types::{CHANNELS, IO_NONE, IO_PORTS, LAYERS};

use format::{Bank, FormatError, SongHeader, NONE16};
use psx_sfx::hw::Hw;
use types::*;

/// Updates per second.
pub const UPDATE_HZ: u32 = 180;

/// Largest number of updates [`Sequencer::pump`] runs in one call.
pub const MAX_CATCH_UP: u32 = 12;

/// Tempo accumulator ceiling: updates per minute. A player at `bpm` runs
/// `bpm * 48 / 60` script ticks per second.
pub(crate) const MAX_TEMPO: u16 = (UPDATE_HZ * 60) as u16;

/// Why a song would not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// No bank is loaded.
    NoBank,
    /// The song container is malformed.
    BadSong(FormatError),
    /// There is no such player.
    NoSuchPlayer,
}

/// The sequencer: `P` players sharing `N` voices.
///
/// Players are independent songs (background music, a fanfare over it, an
/// overlay); each has its own tempo, IO ports and fade, and they compete for
/// the same `N` voices by note priority.
pub struct Sequencer<'a, const P: usize, const N: usize> {
    pub(crate) players: [Player; P],
    pub(crate) codes: [&'a mut [u8]; P],
    pub(crate) notes: [Note; N],
    pub(crate) voice_of: [u8; N],
    pub(crate) bank: Option<Bank<'a>>,
    pub(crate) spu_base: u32,
    pub(crate) master_q12: u16,
    pub(crate) rng: u32,
    pub(crate) order: u32,
    pub(crate) pos_min: i32,
    pub(crate) pos_max: i32,
    pub(crate) unsupported: u32,
    clock: Option<u32>,
}

impl<'a, const P: usize, const N: usize> Sequencer<'a, P, N> {
    /// A sequencer over the voices set in `voice_mask`, which must have
    /// exactly `N` bits set.
    ///
    /// # Panics
    ///
    /// If `voice_mask` does not have `N` bits set.
    pub fn new(voice_mask: u32) -> Self {
        assert_eq!(
            voice_mask.count_ones() as usize,
            N,
            "voice mask must have N bits"
        );
        let mut voice_of = [0u8; N];
        let mut k = 0;
        for v in 0..24u8 {
            if voice_mask & (1 << v) != 0 {
                voice_of[k] = v;
                k += 1;
            }
        }
        let mut s = Self {
            players: core::array::from_fn(|_| Player::init()),
            codes: core::array::from_fn(|_| <&mut [u8]>::default()),
            notes: core::array::from_fn(|_| Note::INIT),
            voice_of,
            bank: None,
            spu_base: 0,
            master_q12: 4096,
            rng: 0x1234_5678,
            order: 0,
            pos_min: 0,
            pos_max: 0,
            unsupported: 0,
            clock: None,
        };
        for p in 0..P {
            s.players[p].enabled = false;
        }
        s
    }

    /// Use `bank` for instruments. `spu_base` is where its ADPCM region was
    /// uploaded in SPU RAM.
    pub fn set_bank(&mut self, bank: Bank<'a>, spu_base: u32) {
        self.bank = Some(bank);
        self.spu_base = spu_base;
    }

    /// The font index in the bank whose id is `id`.
    pub(crate) fn font_index(&self, id: u8) -> Option<u8> {
        self.bank?.font_index(id)
    }

    /// Start `song` on `player`. `song` is the whole container as loaded;
    /// songs that patch themselves need it in RAM.
    pub fn start(&mut self, player: usize, song: &'a mut [u8]) -> Result<(), StartError> {
        if player >= P {
            return Err(StartError::NoSuchPlayer);
        }
        if self.bank.is_none() {
            return Err(StartError::NoBank);
        }
        let header = SongHeader::parse(song).map_err(StartError::BadSong)?;
        self.disable_player(player);
        let code = header.code_mut(song);
        self.codes[player] = code;
        let mut pl = Player::init();
        pl.n_fonts = header.n_fonts;
        pl.fonts = header.font_ids;
        let last = header.font_ids[header.n_fonts.saturating_sub(1) as usize];
        pl.default_font = if header.n_fonts > 0 {
            self.font_index(last).unwrap_or(NONE8)
        } else {
            NONE8
        };
        pl.enabled = true;
        pl.state = 1;
        self.players[player] = pl;
        for c in 0..CHANNELS {
            self.players[player].channels[c].font = self.players[player].default_font;
        }
        Ok(())
    }

    /// Stop `player`, fading its volume to nothing over `fade_updates`
    /// updates first (0 stops now; held notes release at their own rate).
    pub fn stop(&mut self, player: usize, fade_updates: u16) {
        if player >= P || !self.players[player].enabled {
            return;
        }
        if fade_updates == 0 {
            self.disable_player(player);
            return;
        }
        let pl = &mut self.players[player];
        pl.fade_timer = fade_updates;
        pl.state = 2;
        pl.fade_vel_q24 = -pl.fade_q24 / fade_updates as i32;
    }

    /// Is `player` still running its song?
    pub fn is_playing(&self, player: usize) -> bool {
        player < P && self.players[player].enabled
    }

    /// Voices that are not free.
    pub fn voices_in_use(&self) -> u32 {
        self.notes
            .iter()
            .filter(|n| n.list != NoteList::Disabled)
            .count() as u32
    }

    /// Opcodes met so far that this engine does not implement.
    pub fn unsupported_ops(&self) -> u32 {
        self.unsupported
    }

    /// Write a sequence-level IO port (what `ldio` in the song reads).
    pub fn set_player_io(&mut self, player: usize, port: usize, value: u8) {
        if player < P && port < IO_PORTS {
            self.players[player].io[port] = value;
        }
    }

    /// Read back a sequence-level IO port (what `stio` in the song wrote).
    pub fn player_io(&self, player: usize, port: usize) -> Option<u8> {
        let v = *self.players.get(player)?.io.get(port)?;
        (v != IO_NONE).then_some(v)
    }

    /// Write a channel IO port.
    pub fn set_channel_io(&mut self, player: usize, channel: usize, port: usize, value: u8) {
        if player < P && channel < CHANNELS && port < IO_PORTS {
            self.players[player].channels[channel].io[port] = value;
        }
    }

    /// Read back a channel IO port.
    pub fn channel_io(&self, player: usize, channel: usize, port: usize) -> Option<u8> {
        let v = *self
            .players
            .get(player)?
            .channels
            .get(channel)?
            .io
            .get(port)?;
        (v != IO_NONE).then_some(v)
    }

    /// Mute (set bits) or unmute channels of `player`. Muted channels start no
    /// new notes.
    pub fn set_channel_mute(&mut self, player: usize, mask: u16) {
        if player < P {
            for c in 0..CHANNELS {
                self.players[player].channels[c].muted = mask & (1 << c) != 0;
            }
        }
    }

    /// Fade `player`'s volume to `target_q15` (32768 = full) over `updates`.
    pub fn fade_to(&mut self, player: usize, target_q15: u16, updates: u16) {
        if player >= P {
            return;
        }
        let pl = &mut self.players[player];
        let target = (target_q15 as i32) << 9;
        if updates == 0 {
            pl.fade_q24 = target;
            pl.fade_timer = 0;
        } else {
            pl.fade_timer = updates;
            pl.fade_vel_q24 = (target - pl.fade_q24) / updates as i32;
        }
        pl.recalc_volume = true;
    }

    /// Set the output gain applied to every voice (`4096` = 1.0). The game
    /// passes the music volume times the duck gain from `psx_sfx::master`.
    pub fn set_master(&mut self, q12: u16) {
        self.master_q12 = q12;
    }

    /// Mute or unmute a whole player (what the pause menu does). Behaviour
    /// follows the song's mute settings: by default notes stop and the rest is
    /// softened.
    pub fn set_player_muted(&mut self, player: usize, muted: bool) {
        if player < P {
            self.players[player].muted = muted;
            self.players[player].recalc_volume = true;
        }
    }

    /// Run the updates due at `now` (a count of [`UPDATE_HZ`] updates on the
    /// caller's clock). The first call only sets the clock.
    pub fn pump(&mut self, hw: &mut impl Hw, now: u32) {
        let Some(last) = self.clock else {
            self.clock = Some(now);
            return;
        };
        let due = now.wrapping_sub(last);
        if due == 0 || due > u32::MAX / 2 {
            return;
        }
        let run = due.min(MAX_CATCH_UP);
        self.clock = Some(now);
        for _ in 0..run {
            self.update(hw);
        }
    }

    /// Run exactly one update.
    pub fn update(&mut self, hw: &mut impl Hw) {
        self.next_random();
        for p in 0..P {
            if self.players[p].enabled {
                self.step_player(p);
            }
        }
        self.process_notes(hw);
    }

    /// Key off every voice and forget all notes (before overwriting the
    /// sample region in SPU RAM).
    pub fn silence(&mut self, hw: &mut impl Hw) {
        for p in 0..P {
            self.disable_player(p);
        }
        for n in 0..N {
            let v = self.voice_of[n];
            if self.notes[n].keyed {
                hw.set_volume(v, 0, 0);
                hw.key_off(1 << v);
            }
            self.notes[n] = Note::INIT;
        }
    }

    /// Whether the song header says `flag`.
    pub fn song_flags(bytes: &[u8]) -> Result<u16, FormatError> {
        SongHeader::parse(bytes).map(|h| h.flags)
    }
}

#[allow(dead_code)]
const _: u16 = NONE16;

#[cfg(test)]
mod tests;
