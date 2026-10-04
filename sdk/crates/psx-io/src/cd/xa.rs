// SPDX-License-Identifier: GPL-2.0-or-later
//! XA-ADPCM music: stream one channel of an interleaved XA file through the
//! drive's own decoder to the SPU's CD input.
//!
//! The encoder side is `psx-audio-cook xa-encode`; `docs/XA-MUSIC.md` has the
//! workflow. A song file holds several songs as interleaved channels (every
//! fourth sector for four 37.8 kHz stereo songs at single speed). With
//! XA-ADPCM and the file/channel filter switched on, the drive hands the
//! sectors of the chosen channel to its decoder and drops the rest; the CPU
//! never sees the audio.
//!
//! # The drive is busy while music plays
//!
//! Music is a read: the drive streams the file continuously, so the CPU
//! cannot read data from the disc meanwhile, not even at the same time at the
//! same speed (the laser follows the file, not the data). Load first, then
//! start the song, or stop it for the load and start it again afterwards (it
//! restarts from the top). Streaming data and music together needs the data
//! interleaved into the filler slots of the song file, which this player does
//! not do.
//!
//! # Polling
//!
//! The player is poll-driven like [`super::audio`]: call [`Player::poll`]
//! about once per frame. It asks the drive where the head is, and when the
//! head reaches the end of the song file it either restarts the file
//! (looping) or pauses the drive. A restart is a seek, so a loop has an
//! audible gap whose length is the drive's seek time; authoring the loop
//! point into a quiet passage hides it.

use super::{
    set_audio_mixer, try_command, try_pause_until_complete, try_play_position, try_set_mode,
    try_set_target_lba, try_unmute, PlayPosition,
};
use crate::disc_base::shift_lba;
use crate::periph::Cd;
use psx_hw::cd::xa::{END_GUARD_SECTORS, SECTORS_PER_SECOND};
use psx_hw::cd::{CMD_READS, CMD_SETFILTER, MODE_DOUBLE_SPEED, MODE_XA_ADPCM, MODE_XA_FILTER};

/// Per-command response spin budget: generous, the drive answers far sooner.
pub const COMMAND_SPINS: u32 = 131_072;
/// Spin budget for a pause to finish (the drive spends up to about a tenth
/// of a second stopping a read).
const PAUSE_SPINS: u32 = 2_000_000;

/// Speed the song file was interleaved for (`psx-audio-cook xa-encode --speed`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DriveSpeed {
    /// 75 sectors per second.
    Single,
    /// 150 sectors per second.
    Double,
}

impl DriveSpeed {
    const fn sectors_per_second(self) -> u32 {
        match self {
            DriveSpeed::Single => SECTORS_PER_SECOND,
            DriveSpeed::Double => 2 * SECTORS_PER_SECOND,
        }
    }

    const fn mode(self) -> u8 {
        let speed = match self {
            DriveSpeed::Single => 0,
            DriveSpeed::Double => MODE_DOUBLE_SPEED,
        };
        MODE_XA_ADPCM | MODE_XA_FILTER | speed
    }
}

/// An interleaved XA file on the disc.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct File {
    lba: u32,
    sector_count: u32,
    number: u8,
    speed: DriveSpeed,
}

impl File {
    /// A file at `lba` (relative to this program's disc image, see
    /// [`crate::disc_base`]) of `sector_count` sectors, including the end
    /// guard `mkisopsx` keeps. `number` and `speed` come from the encoder's
    /// manifest.
    pub const fn new(lba: u32, sector_count: u32, number: u8, speed: DriveSpeed) -> Self {
        Self {
            lba,
            sector_count,
            number,
            speed,
        }
    }

    /// A file from its ISO 9660 directory record: first LBA and size in
    /// bytes (a whole number of 2048-byte sectors for an XA file).
    pub const fn from_directory_entry(
        lba: u32,
        size_bytes: u32,
        number: u8,
        speed: DriveSpeed,
    ) -> Self {
        Self::new(lba, size_bytes / 2048, number, speed)
    }

    /// Channel `channel` of this file as a song.
    pub const fn song(self, channel: u8) -> Song {
        Song {
            file: self,
            channel,
        }
    }

    /// Sectors the songs span, without the end guard.
    pub const fn span_sector_count(self) -> u32 {
        self.sector_count.saturating_sub(END_GUARD_SECTORS)
    }
}

/// One channel of an [`File`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Song {
    file: File,
    channel: u8,
}

/// The step of starting a song that the drive refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Demute did not answer.
    Unmute,
    /// Setmode did not answer.
    SetMode,
    /// Setfilter did not answer.
    SetFilter,
    /// Setloc did not answer.
    Seek,
    /// ReadS did not answer.
    Read,
}

/// What [`Player::poll`] saw.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// No song is playing.
    Idle,
    /// A song is playing (or its start is still seeking).
    Playing,
    /// The song reached its end and restarted from the top.
    Looped,
    /// The song reached its end and the drive was paused.
    Finished,
}

/// The drive commands the player needs, so the state machine can run against
/// a model on the host.
trait Drive {
    fn unmute(&mut self) -> bool;
    fn set_mode(&mut self, mode: u8) -> bool;
    fn set_filter(&mut self, file: u8, channel: u8) -> bool;
    fn set_target(&mut self, lba: u32) -> bool;
    fn start_streaming(&mut self) -> bool;
    /// Pause the drive and wait for it; `false` if the drive refused (it
    /// does during the seek at the start of a read).
    fn pause(&mut self) -> bool;
    /// Absolute LBA the head is reading, if the drive answers.
    fn head_lba(&mut self) -> Option<u32>;
}

struct Mmio;

impl Drive for Mmio {
    fn unmute(&mut self) -> bool {
        try_unmute(COMMAND_SPINS).is_some()
    }
    fn set_mode(&mut self, mode: u8) -> bool {
        try_set_mode(mode, COMMAND_SPINS).is_some()
    }
    fn set_filter(&mut self, file: u8, channel: u8) -> bool {
        try_command(CMD_SETFILTER, &[file, channel], COMMAND_SPINS).is_some()
    }
    fn set_target(&mut self, lba: u32) -> bool {
        try_set_target_lba(lba, COMMAND_SPINS).is_some()
    }
    fn start_streaming(&mut self) -> bool {
        try_command(CMD_READS, &[], COMMAND_SPINS).is_some()
    }
    fn pause(&mut self) -> bool {
        try_pause_until_complete(PAUSE_SPINS)
    }
    fn head_lba(&mut self) -> Option<u32> {
        let position = PlayPosition::parse(&try_play_position(COMMAND_SPINS)?)?;
        let frames = (position.absolute_min as u32 * 60 + position.absolute_sec as u32) * 75
            + position.absolute_frame as u32;
        // The drive counts from the start of the lead-in, two seconds before LBA 0.
        frames.checked_sub(150)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum State {
    Idle,
    /// Started; the head has not been seen inside the file yet. The drive
    /// reports its old position until the seek lands, so an end test now
    /// would fire on the previous run's position.
    Seeking,
    Streaming,
}

struct Engine {
    state: State,
    song: Option<Song>,
    looping: bool,
    /// A pause was refused; try again at the next poll.
    pause_pending: bool,
    start_lba: u32,
    head_lba: u32,
}

impl Engine {
    const fn new() -> Self {
        Self {
            state: State::Idle,
            song: None,
            looping: false,
            pause_pending: false,
            start_lba: 0,
            head_lba: 0,
        }
    }

    fn start(&mut self, drive: &mut impl Drive, song: Song, looping: bool) -> Result<(), Error> {
        let file = song.file;
        let result = (|| {
            if !drive.unmute() {
                return Err(Error::Unmute);
            }
            if !drive.set_mode(file.speed.mode()) {
                return Err(Error::SetMode);
            }
            if !drive.set_filter(file.number, song.channel) {
                return Err(Error::SetFilter);
            }
            self.begin_read(drive, song)
        })();
        if result.is_ok() {
            self.song = Some(song);
            self.looping = looping;
        } else {
            self.state = State::Idle;
            self.song = None;
        }
        result
    }

    /// Seek to the top of the file and start streaming.
    fn begin_read(&mut self, drive: &mut impl Drive, song: Song) -> Result<(), Error> {
        let start = shift_lba(song.file.lba);
        if !drive.set_target(start) {
            return Err(Error::Seek);
        }
        if !drive.start_streaming() {
            return Err(Error::Read);
        }
        self.start_lba = start;
        self.head_lba = start;
        self.pause_pending = false;
        self.state = State::Seeking;
        Ok(())
    }

    /// Pause a running drive. The drive refuses a pause during the seek that
    /// starts a read, so a refused pause is retried by [`Self::poll`].
    fn stop(&mut self, drive: &mut impl Drive) {
        if self.state != State::Idle {
            self.pause_pending = !drive.pause();
        }
        self.state = State::Idle;
        self.song = None;
    }

    fn poll(&mut self, drive: &mut impl Drive) -> Event {
        let Some(song) = self.song else {
            if self.pause_pending {
                self.pause_pending = !drive.pause();
            }
            return Event::Idle;
        };
        let end = self.start_lba + song.file.span_sector_count();
        if let Some(head) = drive.head_lba() {
            if self.state == State::Seeking && (self.start_lba..end).contains(&head) {
                self.state = State::Streaming;
            }
            if self.state == State::Streaming {
                self.head_lba = head;
            }
            if self.state == State::Streaming && head >= end {
                return self.reach_end(drive, song);
            }
        }
        Event::Playing
    }

    fn is_streaming(&self) -> bool {
        self.state == State::Streaming
    }

    fn elapsed_millis(&self) -> u32 {
        let Some(song) = self.song else {
            return 0;
        };
        let sectors = self.head_lba.saturating_sub(self.start_lba);
        sectors * 1000 / song.file.speed.sectors_per_second()
    }

    fn reach_end(&mut self, drive: &mut impl Drive, song: Song) -> Event {
        if self.looping && self.begin_read(drive, song).is_ok() {
            return Event::Looped;
        }
        self.stop(drive);
        Event::Finished
    }
}

/// Plays XA-ADPCM songs. Owns the CD token: nothing else drives the
/// controller while a player exists.
pub struct Player {
    _cd: Cd,
    engine: Engine,
}

impl Player {
    /// A player with nothing playing. The SPU's CD input must be on and have
    /// a volume (`psx_spu::enable_cd_audio`, `psx_spu::set_cd_volume`), and
    /// the drive's own mixer must pass audio ([`set_volume`](Self::set_volume)).
    pub const fn new(cd: Cd) -> Self {
        Self {
            _cd: cd,
            engine: Engine::new(),
        }
    }

    /// Stop and give the token back.
    pub fn release(mut self) -> Cd {
        self.stop();
        self._cd
    }

    /// Start `song` from the top (or restart it). With `looping` it
    /// restarts by itself when [`poll`](Self::poll) sees it end. Blocks for
    /// the five drive commands, a few milliseconds on a warm drive.
    pub fn play(&mut self, song: Song, looping: bool) -> Result<(), Error> {
        self.engine.start(&mut Mmio, song, looping)
    }

    /// Pause the drive and forget the song. Waits for the drive to settle.
    pub fn stop(&mut self) {
        self.engine.stop(&mut Mmio);
    }

    /// Call about once per frame: notices the end of the song and loops or
    /// stops. Costs one drive command.
    pub fn poll(&mut self) -> Event {
        self.engine.poll(&mut Mmio)
    }

    /// Whether a song is started and has not finished.
    pub fn is_playing(&self) -> bool {
        self.engine.state != State::Idle
    }

    /// Whether the head has been seen inside the song file since the last
    /// start, so the audio is flowing and not still seeking.
    pub fn is_streaming(&self) -> bool {
        self.engine.is_streaming()
    }

    /// The song being played.
    pub fn song(&self) -> Option<Song> {
        self.engine.song
    }

    /// Time since the start of the song file at the last poll, in
    /// milliseconds (the head runs a sector or two ahead of the sound).
    pub fn elapsed_millis(&self) -> u32 {
        self.engine.elapsed_millis()
    }

    /// Set the level of the drive's audio output (CD-DA too). `0x80` is
    /// unity, `0xFF` about twice that; `(left, right)` feed the left and
    /// right outputs of a stereo song.
    pub fn set_volume(&mut self, left: u8, right: u8) {
        set_audio_mixer(left, 0, right, 0);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;
    use std::{format, vec};

    /// Records commands; `heads` is what GetlocP answers, one per poll.
    #[derive(Default)]
    struct Fake {
        log: Vec<String>,
        fail_at: Option<&'static str>,
        heads: Vec<Option<u32>>,
        pause_refusals: u32,
    }

    impl Fake {
        fn step(&mut self, name: &str) -> bool {
            self.log.push(name.to_string());
            self.fail_at
                != Some(match name {
                    n if n.starts_with("unmute") => "unmute",
                    n if n.starts_with("mode") => "mode",
                    n if n.starts_with("filter") => "filter",
                    n if n.starts_with("target") => "target",
                    _ => "read",
                })
        }
    }

    impl Drive for Fake {
        fn unmute(&mut self) -> bool {
            self.step("unmute")
        }
        fn set_mode(&mut self, mode: u8) -> bool {
            self.step(&format!("mode {mode:#04x}"))
        }
        fn set_filter(&mut self, file: u8, channel: u8) -> bool {
            self.step(&format!("filter {file} {channel}"))
        }
        fn set_target(&mut self, lba: u32) -> bool {
            self.step(&format!("target {lba}"))
        }
        fn start_streaming(&mut self) -> bool {
            self.step("read")
        }
        fn pause(&mut self) -> bool {
            self.log.push("pause".into());
            if self.pause_refusals > 0 {
                self.pause_refusals -= 1;
                return false;
            }
            true
        }
        fn head_lba(&mut self) -> Option<u32> {
            if self.heads.is_empty() {
                None
            } else {
                self.heads.remove(0)
            }
        }
    }

    /// 100 sectors of songs plus the guard, at LBA 1000.
    fn song(speed: DriveSpeed) -> Song {
        File::new(1000, 100 + END_GUARD_SECTORS, 3, speed).song(2)
    }

    #[test]
    fn start_sets_up_the_filter_before_the_read() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), true)
            .unwrap();
        assert_eq!(
            drive.log,
            ["unmute", "mode 0x48", "filter 3 2", "target 1000", "read"]
        );
        let mut drive = Fake::default();
        engine
            .start(&mut drive, song(DriveSpeed::Double), true)
            .unwrap();
        assert_eq!(drive.log[1], "mode 0xc8");
    }

    #[test]
    fn each_refused_step_is_reported_and_leaves_the_player_idle() {
        for (at, error) in [
            ("unmute", Error::Unmute),
            ("mode", Error::SetMode),
            ("filter", Error::SetFilter),
            ("target", Error::Seek),
            ("read", Error::Read),
        ] {
            let mut drive = Fake {
                fail_at: Some(at),
                ..Fake::default()
            };
            let mut engine = Engine::new();
            let result = engine.start(&mut drive, song(DriveSpeed::Single), false);
            assert_eq!(result, Err(error));
            assert_eq!(engine.poll(&mut drive), Event::Idle);
        }
    }

    #[test]
    fn an_old_head_position_does_not_end_a_fresh_start() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Double), true)
            .unwrap();
        drive.log.clear();
        // Past the end (where the previous run stopped), then inside the file.
        drive.heads = vec![Some(1105), None, Some(1003), Some(1050)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(!engine.is_streaming());
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.is_streaming());
        assert_eq!(engine.elapsed_millis(), 3 * 1000 / 150);
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.elapsed_millis(), 50 * 1000 / 150);
        assert!(
            drive.log.is_empty(),
            "no command beyond GetlocP: {:?}",
            drive.log
        );
    }

    #[test]
    fn a_loop_restarts_at_the_top_and_waits_for_the_head_again() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), true)
            .unwrap();
        drive.log.clear();
        drive.heads = vec![Some(1010), Some(1100), Some(1103), Some(1020)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.poll(&mut drive), Event::Looped);
        assert_eq!(drive.log, ["target 1000", "read"]);
        // The drive still reports the guard region until the seek lands.
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(drive.log.len(), 2);
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.is_streaming());
    }

    #[test]
    fn a_one_shot_pauses_at_the_end_and_goes_idle() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.log.clear();
        drive.heads = vec![Some(1099), Some(1101)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.poll(&mut drive), Event::Finished);
        assert_eq!(drive.log, ["pause"]);
        assert_eq!(engine.poll(&mut drive), Event::Idle);
    }

    #[test]
    fn stop_pauses_only_a_running_player() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine.stop(&mut drive);
        assert!(drive.log.is_empty());
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        engine.stop(&mut drive);
        assert_eq!(drive.log.last().unwrap(), "pause");
        assert_eq!(engine.poll(&mut drive), Event::Idle);
    }

    #[test]
    fn a_refused_pause_is_retried_at_the_next_polls() {
        let mut drive = Fake {
            pause_refusals: 2,
            ..Fake::default()
        };
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.log.clear();
        engine.stop(&mut drive);
        assert_eq!(engine.poll(&mut drive), Event::Idle);
        assert_eq!(engine.poll(&mut drive), Event::Idle);
        assert_eq!(drive.log, ["pause", "pause", "pause"]);
        // Accepted: no further commands.
        assert_eq!(engine.poll(&mut drive), Event::Idle);
        assert_eq!(drive.log.len(), 3);
    }

    #[test]
    fn starting_a_song_cancels_a_pending_pause() {
        let mut drive = Fake {
            pause_refusals: 5,
            ..Fake::default()
        };
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        engine.stop(&mut drive);
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.log.clear();
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(drive.log.is_empty());
    }

    #[test]
    fn a_failed_loop_restart_finishes_the_song() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), true)
            .unwrap();
        drive.heads = vec![Some(1010), Some(1100)];
        drive.fail_at = Some("target");
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.poll(&mut drive), Event::Finished);
        assert_eq!(drive.log.last().unwrap(), "pause");
    }

    #[test]
    fn the_file_span_excludes_the_guard() {
        let file = File::from_directory_entry(7, 2048 * 116, 1, DriveSpeed::Single);
        assert_eq!(file.span_sector_count(), 100);
        assert_eq!(
            File::new(0, 4, 0, DriveSpeed::Double).span_sector_count(),
            0
        );
    }
}
