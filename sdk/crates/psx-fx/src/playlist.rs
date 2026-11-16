//! A shuffled song order with no hardware in it.
//!
//! The songs play in one random permutation and none repeats until every song has been
//! through. When the permutation is spent a new one is dealt from the same xorshift32 stream,
//! and its first song is never the one just heard, so the repeat a reshuffle allows between
//! two rounds cannot happen back to back. The order is a function of the seed alone, so host
//! tests can check it.
//!
//! The draw and the swap are NitroXide's (its song order for a seed is unchanged); the
//! launcher's order for a seed moves, which nothing pins since both seed from the boot timers.
//!
//! A menu that wants a way to mute without a button of its own asks for [`OffSlot::After`]:
//! the shoulder buttons then walk every song and on into [`Slot::Off`] before a fresh
//! shuffle, and a song that ends by itself goes straight to the next song and never lands on
//! `Off`. A menu without one asks for [`OffSlot::Without`] and the order wraps.
//!
//! # Example
//!
//! ```
//! use psx_fx::playlist::{OffSlot, Playlist, Slot};
//!
//! let mut list = Playlist::new(3, 0x2545_f491, OffSlot::After);
//! let Slot::Song(first) = list.slot() else { unreachable!() };
//! // Three songs, then MUSIC OFF, then a new shuffle that does not open on the last song.
//! let mut heard = [first, 0, 0];
//! for s in &mut heard[1..] {
//!     let Slot::Song(n) = list.skip_forward() else { unreachable!() };
//!     *s = n;
//! }
//! assert_eq!(list.skip_forward(), Slot::Off);
//! let Slot::Song(next) = list.skip_forward() else { unreachable!() };
//! assert_ne!(next, heard[2]);
//! ```

use crate::rng::Xorshift32;

/// Songs a playlist can hold.
pub const MAX_SONGS: usize = 8;

/// What the music is doing at a playlist position.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Song `n`, numbered as the caller's table is.
    Song(u8),
    /// Silence: the slot after the last song of a [`OffSlot::After`] playlist, or the only slot
    /// of an empty playlist.
    Off,
}

/// Whether a silent slot follows the last song.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OffSlot {
    /// The order wraps from the last song to the first.
    Without,
    /// The slot after the last song is [`Slot::Off`].
    After,
}

/// A shuffled order over `count` songs, and where in it the music stands.
#[derive(Copy, Clone, Debug)]
pub struct Playlist {
    order: [u8; MAX_SONGS],
    count: u8,
    /// `0..count` is a song; `count` is the off slot when there is one.
    pos: u8,
    off: OffSlot,
    rng: u32,
}

impl Playlist {
    /// A playlist over songs `0..count` (at most [`MAX_SONGS`]) dealt from `seed`, positioned on
    /// its first song. A zero `seed` is replaced, since xorshift32 never leaves zero.
    #[must_use]
    pub fn new(count: usize, seed: u32, off: OffSlot) -> Self {
        let mut list = Self {
            order: [0; MAX_SONGS],
            count: count.min(MAX_SONGS) as u8,
            pos: 0,
            off,
            rng: if seed == 0 { 0x9e37_79b9 } else { seed },
        };
        list.shuffle(None);
        list
    }

    /// Number of songs.
    #[must_use]
    pub fn count(&self) -> u8 {
        self.count
    }

    /// Where the playlist stands.
    #[must_use]
    #[inline]
    pub fn slot(&self) -> Slot {
        if self.pos >= self.count {
            Slot::Off
        } else {
            Slot::Song(self.order[self.pos as usize])
        }
    }

    /// Positions a player can stand on: the songs, and the off slot if there is one.
    fn slots(&self) -> u8 {
        self.count + u8::from(self.off == OffSlot::After)
    }

    /// The shoulder button forward: the next song, then the off slot if there is one, then a
    /// fresh shuffle from its first song.
    #[inline(never)]
    pub fn skip_forward(&mut self) -> Slot {
        if self.count == 0 {
            return Slot::Off;
        }
        if self.pos + 1 < self.slots() {
            self.pos += 1;
        } else {
            self.reshuffle();
        }
        self.slot()
    }

    /// The shoulder button back: the previous slot of this shuffle, wrapping to the last slot
    /// (the off slot if there is one, else the last song) from the first.
    #[inline(never)]
    pub fn skip_back(&mut self) -> Slot {
        if self.count == 0 {
            return Slot::Off;
        }
        self.pos = if self.pos == 0 {
            self.slots() - 1
        } else {
            self.pos - 1
        };
        self.slot()
    }

    /// The song ended by itself: on to the next song, reshuffling (without opening on the song
    /// just heard) when this shuffle is spent. Never lands on the off slot.
    #[inline(never)]
    pub fn song_finished(&mut self) -> Slot {
        if self.count == 0 {
            return Slot::Off;
        }
        if self.pos + 1 < self.count {
            self.pos += 1;
        } else {
            self.reshuffle();
        }
        self.slot()
    }

    fn reshuffle(&mut self) {
        let last = self.order[self.count as usize - 1];
        self.shuffle(Some(last));
        self.pos = 0;
    }

    /// Deal a new permutation (Fisher-Yates). With `avoid_first`, the song that just finished
    /// never opens it: a collision swaps the first song with the second (a round of one has no
    /// choice and repeats). The draw uses the stream's upper 24 bits, whose low bits are the
    /// stream's weakest.
    fn shuffle(&mut self, avoid_first: Option<u8>) {
        let n = self.count as usize;
        for (i, slot) in self.order.iter_mut().enumerate().take(n) {
            *slot = i as u8;
        }
        for i in (1..n).rev() {
            self.rng = Xorshift32::step(self.rng);
            let j = ((self.rng >> 8) % (i as u32 + 1)) as usize;
            self.order.swap(i, j);
        }
        if n > 1 && Some(self.order[0]) == avoid_first {
            self.order.swap(0, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NitroXide's playlist as it shipped (`sim/src/playlist.rs`), the oracle for the wrapping
    /// form: the draw, the swap and the stream are the rule this module keeps.
    #[derive(Clone)]
    struct Nitroxide {
        order: [u8; MAX_SONGS],
        len: u8,
        pos: u8,
        rng: u32,
    }

    impl Nitroxide {
        fn new(len: usize, seed: u32) -> Self {
            let len = len.clamp(1, MAX_SONGS) as u8;
            let mut list = Self {
                order: [0; MAX_SONGS],
                len,
                pos: 0,
                rng: if seed == 0 { 0x9e37_79b9 } else { seed },
            };
            list.shuffle(None);
            list
        }
        fn current(&self) -> usize {
            self.order[self.pos as usize] as usize
        }
        fn next(&mut self) -> usize {
            if self.pos + 1 < self.len {
                self.pos += 1;
            } else {
                let last = self.current();
                self.shuffle(Some(last));
                self.pos = 0;
            }
            self.current()
        }
        fn prev(&mut self) -> usize {
            self.pos = if self.pos == 0 {
                self.len - 1
            } else {
                self.pos - 1
            };
            self.current()
        }
        fn step(&mut self) -> u32 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 17;
            self.rng ^= self.rng << 5;
            self.rng
        }
        fn shuffle(&mut self, not_first: Option<usize>) {
            let n = self.len as usize;
            for i in 0..n {
                self.order[i] = i as u8;
            }
            for i in (1..n).rev() {
                let j = (self.step() >> 8) as usize % (i + 1);
                self.order.swap(i, j);
            }
            if n > 1 && not_first == Some(self.order[0] as usize) {
                self.order.swap(0, 1);
            }
        }
    }

    #[test]
    fn without_an_off_slot_it_is_nitroxides_playlist_step_for_step() {
        let mut state = 0x1357_9bdfu32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let mut steps = 0u32;
        for count in 1..=9usize {
            let mut seeds = [0u32; 64];
            seeds[1] = 1;
            seeds[2] = u32::MAX;
            for s in seeds.iter_mut().skip(3) {
                *s = next();
            }
            for seed in seeds {
                let mut old = Nitroxide::new(count, seed);
                let mut new = Playlist::new(count, seed, OffSlot::Without);
                assert_eq!(new.slot(), Slot::Song(old.current() as u8));
                for _ in 0..400 {
                    let (a, b) = match next() % 4 {
                        0 => (old.next(), new.skip_forward()),
                        1 => (old.next(), new.song_finished()),
                        _ => (old.prev(), new.skip_back()),
                    };
                    assert_eq!(Slot::Song(a as u8), b, "count {count} seed {seed:#x}");
                    steps += 1;
                }
            }
        }
        assert!(steps > 200_000);
    }

    #[test]
    fn the_shoulder_buttons_walk_every_song_then_off() {
        let mut list = Playlist::new(4, 7, OffSlot::After);
        let mut songs = [false; 4];
        let Slot::Song(first) = list.slot() else {
            panic!()
        };
        songs[first as usize] = true;
        for _ in 0..3 {
            let Slot::Song(n) = list.skip_forward() else {
                panic!("three more songs before off")
            };
            assert!(!songs[n as usize]);
            songs[n as usize] = true;
        }
        assert_eq!(list.skip_forward(), Slot::Off);
        assert!(
            matches!(list.skip_forward(), Slot::Song(_)),
            "off then a fresh shuffle"
        );
    }

    #[test]
    fn back_from_the_first_song_is_off_and_back_from_off_is_the_last_song() {
        let mut list = Playlist::new(3, 11, OffSlot::After);
        let first = list.slot();
        assert_eq!(list.skip_back(), Slot::Off);
        let last = list.skip_back();
        assert!(matches!(last, Slot::Song(_)));
        assert_ne!(last, first);
        list.skip_back();
        list.skip_back();
        assert_eq!(list.slot(), first);
    }

    #[test]
    fn a_finished_song_never_lands_on_off_even_from_the_off_slot() {
        let mut list = Playlist::new(3, 21, OffSlot::After);
        for _ in 0..3 {
            list.skip_forward();
        }
        assert_eq!(list.slot(), Slot::Off);
        assert!(matches!(list.song_finished(), Slot::Song(_)));
    }

    fn songs(list: &mut Playlist, n: usize) -> [u8; MAX_SONGS] {
        let mut heard = [0u8; MAX_SONGS];
        let Slot::Song(first) = list.slot() else {
            panic!("starts on a song")
        };
        heard[0] = first;
        for s in heard.iter_mut().take(n).skip(1) {
            let Slot::Song(x) = list.song_finished() else {
                panic!("never off")
            };
            *s = x;
        }
        heard
    }

    #[test]
    fn every_song_plays_once_before_any_repeats() {
        for off in [OffSlot::Without, OffSlot::After] {
            for seed in 1..300u32 {
                let mut list = Playlist::new(6, seed.wrapping_mul(0x9e37_79b1), off);
                for _ in 0..4 {
                    let round = songs(&mut list, 6);
                    let mut seen = [false; MAX_SONGS];
                    for &n in &round[..6] {
                        assert!(!seen[n as usize], "seed {seed}: {round:?}");
                        seen[n as usize] = true;
                    }
                    list.song_finished();
                }
            }
        }
    }

    #[test]
    fn a_new_shuffle_never_opens_with_the_song_just_heard() {
        for off in [OffSlot::Without, OffSlot::After] {
            for seed in 1..2000u32 {
                let mut list = Playlist::new(6, seed, off);
                let Slot::Song(mut last) = list.slot() else {
                    panic!()
                };
                for _ in 0..60 {
                    let Slot::Song(now) = list.song_finished() else {
                        panic!()
                    };
                    assert_ne!(now, last, "seed {seed}");
                    last = now;
                }
            }
        }
    }

    #[test]
    fn without_an_off_slot_back_walks_what_forward_walked_and_wraps() {
        let mut list = Playlist::new(6, 77, OffSlot::Without);
        let mut forward = [Slot::Off; 6];
        forward[0] = list.slot();
        for s in forward.iter_mut().skip(1) {
            *s = list.skip_forward();
        }
        assert!(forward.iter().all(|s| matches!(s, Slot::Song(_))));
        for want in forward.iter().rev().skip(1) {
            assert_eq!(list.skip_back(), *want);
        }
        assert_eq!(list.skip_back(), forward[5]);
        // Forward from the last song starts a new shuffle, which does not open on it.
        assert!(matches!(list.skip_forward(), Slot::Song(n) if Slot::Song(n) != forward[5]));
    }

    #[test]
    fn a_single_song_repeats_and_no_songs_is_off() {
        for off in [OffSlot::Without, OffSlot::After] {
            let mut one = Playlist::new(1, 5, off);
            assert_eq!(one.slot(), Slot::Song(0));
            assert_eq!(one.song_finished(), Slot::Song(0));
            let mut none = Playlist::new(0, 5, off);
            assert_eq!(none.slot(), Slot::Off);
            assert_eq!(none.song_finished(), Slot::Off);
            assert_eq!(none.skip_forward(), Slot::Off);
            assert_eq!(none.skip_back(), Slot::Off);
            assert_eq!(Playlist::new(40, 5, off).count() as usize, MAX_SONGS);
        }
    }

    #[test]
    fn different_seeds_deal_different_orders_and_zero_still_shuffles() {
        let a = songs(&mut Playlist::new(6, 1, OffSlot::Without), 6);
        assert!((2..40).any(|seed| songs(&mut Playlist::new(6, seed, OffSlot::Without), 6) != a));
        let z = songs(&mut Playlist::new(6, 0, OffSlot::Without), 6);
        let mut seen = [false; MAX_SONGS];
        for &n in &z[..6] {
            seen[n as usize] = true;
        }
        assert!(seen[..6].iter().all(|&s| s));
    }
}
