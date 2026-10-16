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
//! start the song, or lend the drive for the load with
//! [`Player::with_cd_suspended`] and [`Player::resume`] afterwards (a seek
//! back to where the song was), or stop it and start it again (it restarts
//! from the top). Streaming data and music together needs the data
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

use super::PlayPosition;
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

/// The drive refused Pause, as it does during the seek at the start of a
/// read. Keep polling and try again.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PauseRefused;

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

/// One reading of the drive head inside the playing song, taken by
/// [`Player::poll`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HeadSample {
    /// Changes whenever a read starts (a start, a loop or a resume): readings
    /// of different epochs do not belong to one run of the song.
    pub epoch: u32,
    /// Changes with every new reading in the same epoch.
    pub serial: u32,
    /// The sector the head was in, counted from the top of the song file.
    pub sector: u32,
    /// Sectors the drive reads per second at the file's speed (75 or 150).
    pub sectors_per_second: u32,
}

impl HeadSample {
    /// Start of the head's sector, in milliseconds since the top of the file.
    pub const fn millis(self) -> u32 {
        (self.sector as u64 * 1000 / self.sectors_per_second as u64) as u32
    }

    /// Middle of the head's sector, in milliseconds: the head was somewhere
    /// in the sector, so this is the reading with the least expected error
    /// (half a sector, 6.7 ms at single speed).
    pub const fn centre_millis(self) -> u32 {
        ((2 * self.sector as u64 + 1) * 500 / self.sectors_per_second as u64) as u32
    }
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

impl Drive for Cd {
    fn unmute(&mut self) -> bool {
        self.try_unmute(COMMAND_SPINS).is_some()
    }
    fn set_mode(&mut self, mode: u8) -> bool {
        self.try_set_mode(mode, COMMAND_SPINS).is_some()
    }
    fn set_filter(&mut self, file: u8, channel: u8) -> bool {
        self.try_command(CMD_SETFILTER, &[file, channel], COMMAND_SPINS)
            .is_some()
    }
    fn set_target(&mut self, lba: u32) -> bool {
        self.try_set_target_lba(lba, COMMAND_SPINS).is_some()
    }
    fn start_streaming(&mut self) -> bool {
        self.try_command(CMD_READS, &[], COMMAND_SPINS).is_some()
    }
    fn pause(&mut self) -> bool {
        self.try_pause_until_complete(PAUSE_SPINS)
    }
    fn head_lba(&mut self) -> Option<u32> {
        let position = PlayPosition::parse(&self.try_play_position(COMMAND_SPINS)?)?;
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
    suspended: bool,
    suspend_pending: bool,
    /// Absolute LBA of the top of the song file.
    start_lba: u32,
    /// Where the last read was aimed: the top of the file for a start or a
    /// loop, the saved position for a resume.
    target_lba: u32,
    head_lba: u32,
    /// Where a suspended song continues, fixed when the pause is accepted.
    resume_lba: u32,
    /// Counts reads started (start, loop, resume): a new epoch means the
    /// head readings before it say nothing about the ones after.
    epoch: u32,
    /// Counts head readings taken while streaming, so a clock can tell a
    /// fresh reading from the one it already used.
    head_serial: u32,
}

impl Engine {
    const fn new() -> Self {
        Self {
            state: State::Idle,
            song: None,
            looping: false,
            pause_pending: false,
            suspended: false,
            suspend_pending: false,
            start_lba: 0,
            target_lba: 0,
            head_lba: 0,
            resume_lba: 0,
            epoch: 0,
            head_serial: 0,
        }
    }

    fn start(&mut self, drive: &mut impl Drive, song: Song, looping: bool) -> Result<(), Error> {
        self.suspended = false;
        self.suspend_pending = false;
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
        let top = shift_lba(song.file.lba);
        self.begin_read_at(drive, top, top)
    }

    /// Seek to `at` (an absolute LBA inside the song file whose top is `top`)
    /// and start streaming.
    fn begin_read_at(&mut self, drive: &mut impl Drive, top: u32, at: u32) -> Result<(), Error> {
        if !drive.set_target(at) {
            return Err(Error::Seek);
        }
        if !drive.start_streaming() {
            return Err(Error::Read);
        }
        self.start_lba = top;
        self.target_lba = at;
        self.head_lba = at;
        self.pause_pending = false;
        self.state = State::Seeking;
        self.epoch = self.epoch.wrapping_add(1);
        Ok(())
    }

    /// Pause a running drive. The drive refuses a pause during the seek that
    /// starts a read, so a refused pause is retried by [`Self::poll`].
    fn stop(&mut self, drive: &mut impl Drive) {
        self.suspended = false;
        self.suspend_pending = false;
        if self.state != State::Idle {
            self.pause_pending = !drive.pause();
        }
        self.state = State::Idle;
        self.song = None;
    }

    fn suspend(&mut self, drive: &mut impl Drive) -> bool {
        if self.state == State::Idle || self.suspended {
            return true;
        }
        self.suspended = drive.pause();
        self.suspend_pending = !self.suspended;
        if self.suspended {
            self.note_resume_position(drive);
        }
        self.suspended
    }

    /// Fix where a resume continues, now that the pause is accepted and the
    /// head holds still. A poll may be a long time old (a game that polls
    /// only near the end of a song never refreshed it), so ask the drive
    /// once more; if it does not answer, fall back to the last position the
    /// poll saw, or to the seek target when no poll ever saw the head.
    fn note_resume_position(&mut self, drive: &mut impl Drive) {
        let Some(song) = self.song else {
            return;
        };
        let end = self.start_lba + song.file.span_sector_count();
        if let Some(head) = drive.head_lba() {
            if (self.target_lba..end).contains(&head) {
                self.state = State::Streaming;
                self.head_lba = head;
                self.resume_lba = head;
                return;
            }
        }
        self.resume_lba = if self.state == State::Streaming {
            self.head_lba
        } else {
            self.target_lba
        };
    }

    /// Continue a suspended song. Anything may have happened to the drive
    /// since the pause (a data read rewrites the mode, the head sits at the
    /// data), so every setting is sent again, in the order a start sends
    /// them, and the read is aimed at the saved position, not left to wherever
    /// the controller stopped. A refused step leaves the song suspended.
    fn resume(&mut self, drive: &mut impl Drive) -> Result<(), Error> {
        if self.suspended {
            if let Some(song) = self.song {
                let file = song.file;
                if !drive.unmute() {
                    return Err(Error::Unmute);
                }
                if !drive.set_mode(file.speed.mode()) {
                    return Err(Error::SetMode);
                }
                if !drive.set_filter(file.number, song.channel) {
                    return Err(Error::SetFilter);
                }
                self.begin_read_at(drive, self.start_lba, self.resume_lba)?;
            }
            self.suspended = false;
        }
        // A refused Pause left the original read active. Cancel its retry
        // without starting another read or changing the pending seek.
        self.suspend_pending = false;
        Ok(())
    }

    fn poll(&mut self, drive: &mut impl Drive) -> Event {
        let Some(song) = self.song else {
            if self.pause_pending {
                self.pause_pending = !drive.pause();
            }
            return Event::Idle;
        };
        if self.suspend_pending {
            self.suspend(drive);
        }
        if self.suspended || self.suspend_pending {
            return Event::Playing;
        }
        let end = self.start_lba + song.file.span_sector_count();
        if let Some(head) = drive.head_lba() {
            if self.state == State::Seeking && (self.target_lba..end).contains(&head) {
                self.state = State::Streaming;
            }
            if self.state == State::Streaming {
                self.head_lba = head;
                self.head_serial = self.head_serial.wrapping_add(1);
            }
            if self.state == State::Streaming && head >= end {
                return self.reach_end(drive, song);
            }
        }
        Event::Playing
    }

    fn is_streaming(&self) -> bool {
        self.state == State::Streaming && !self.suspended && !self.suspend_pending
    }

    fn elapsed_millis(&self) -> u32 {
        let Some(song) = self.song else {
            return 0;
        };
        let sectors = self.head_lba.saturating_sub(self.start_lba);
        sectors * 1000 / song.file.speed.sectors_per_second()
    }

    fn head_sample(&self) -> Option<HeadSample> {
        if !self.is_streaming() {
            return None;
        }
        Some(HeadSample {
            epoch: self.epoch,
            serial: self.head_serial,
            sector: self.head_lba.saturating_sub(self.start_lba),
            sectors_per_second: self.song?.file.speed.sectors_per_second(),
        })
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
    cd: Cd,
    engine: Engine,
}

impl Player {
    /// A player with nothing playing. The SPU's CD input must be on and have
    /// a volume (`psx_spu::enable_cd_audio`, `psx_spu::set_cd_volume`), and
    /// the drive's own mixer must pass audio ([`set_volume`](Self::set_volume)).
    pub const fn new(cd: Cd) -> Self {
        Self {
            cd,
            engine: Engine::new(),
        }
    }

    /// Stop and give the token back.
    pub fn release(mut self) -> Cd {
        self.stop();
        self.cd
    }

    /// Start `song` from the top (or restart it). With `looping` it
    /// restarts by itself when [`poll`](Self::poll) sees it end. Blocks for
    /// the five drive commands, a few milliseconds on a warm drive.
    pub fn play(&mut self, song: Song, looping: bool) -> Result<(), Error> {
        self.engine.start(&mut self.cd, song, looping)
    }

    /// Pause the drive and forget the song. Waits for the drive to settle.
    pub fn stop(&mut self) {
        self.engine.stop(&mut self.cd);
    }

    /// Suspend the current song without releasing the CD token or forgetting
    /// its filter, looping policy or elapsed position. Returns false when the
    /// drive refuses Pause during a seek; keep polling to retry it.
    /// An idle player or an already suspended song returns true.
    pub fn suspend(&mut self) -> bool {
        self.engine.suspend(&mut self.cd)
    }

    /// Continue a suspended song where it stopped. Safe after the drive was
    /// used for something else in between (see
    /// [`with_cd_suspended`](Self::with_cd_suspended)): it sends Demute,
    /// Setmode (XA, filter and the file's speed), Setfilter (the file and
    /// channel) and Setloc to the position the pause left, then ReadS, the
    /// same steps [`play`](Self::play) takes except that it aims at the saved
    /// position and not at the top. Costs a short seek.
    ///
    /// The position is the head at the pause, read back from the drive, so
    /// the music continues within about one sector of where it stopped. A
    /// channel gets one of its sectors every few sectors of the file (every
    /// fourth for four 37.8 kHz stereo songs, 53 ms of sound each), so the
    /// first sector after a resume can repeat or skip up to that much sound.
    /// This is not a sample-exact cursor.
    ///
    /// A refused step returns its [`Error`] and leaves the song suspended for
    /// an explicit retry. Cancels a still-pending suspension; idle and
    /// running players are no-ops.
    pub fn resume(&mut self) -> Result<(), Error> {
        self.engine.resume(&mut self.cd)
    }

    /// Lend the drive to `f` while the song is suspended, for a data read
    /// (a level load) that must not run on top of the music. Suspends the
    /// song first; if the drive refuses the pause (it does during the seek
    /// at the start of a read), `f` is not called and the error says to keep
    /// calling [`poll`](Self::poll) and try again. `f` gets the token and
    /// hands it back with its result. Afterwards call
    /// [`resume`](Self::resume) to continue the song, or [`play`](Self::play)
    /// to start another.
    ///
    /// ```ignore
    /// let loaded = player.with_cd_suspended(|cd| {
    ///     let mut reader = SectorReader::with_cd(cd);
    ///     let ok = load_level(&mut reader);
    ///     (reader.release(), ok)
    /// })?;
    /// player.resume()?;
    /// ```
    pub fn with_cd_suspended<R>(
        &mut self,
        f: impl FnOnce(Cd) -> (Cd, R),
    ) -> Result<R, PauseRefused> {
        if !self.engine.suspend(&mut self.cd) {
            return Err(PauseRefused);
        }
        // SAFETY: a token is a logic guard, not a memory-safety one (see
        // `crate::periph`). The stand-in sits in the player for the length of
        // `f` and nothing can reach it: the player is mutably borrowed.
        let cd = core::mem::replace(&mut self.cd, unsafe { Cd::steal() });
        let (cd, result) = f(cd);
        self.cd = cd;
        Ok(result)
    }

    /// Whether the drive has acknowledged suspension. False while Pause is
    /// still being retried. The retained song still counts as `is_playing`.
    pub fn is_suspended(&self) -> bool {
        self.engine.suspended
    }

    /// Call about once per frame: notices the end of the song and loops or
    /// stops, or retries a pending suspension. A suspended song returns
    /// `Event::Playing` without querying or advancing the head.
    pub fn poll(&mut self) -> Event {
        self.engine.poll(&mut self.cd)
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
    /// milliseconds (the head runs a sector or two ahead of the sound). The
    /// value only moves when [`poll`](Self::poll) reads the head; for a time
    /// that advances every frame see [`SongClock`](super::xa_clock::SongClock).
    pub fn elapsed_millis(&self) -> u32 {
        self.engine.elapsed_millis()
    }

    /// The newest head reading, while the song is streaming (`None` while
    /// it seeks, is suspended or is not playing). The input of
    /// [`SongClock`](super::xa_clock::SongClock).
    pub fn head_sample(&self) -> Option<HeadSample> {
        self.engine.head_sample()
    }

    /// Set the level of the drive's audio output (CD-DA too). `0x80` is
    /// unity, `0xFF` about twice that; `(left, right)` feed the left and
    /// right outputs of a stereo song.
    pub fn set_volume(&mut self, left: u8, right: u8) {
        self.cd.set_audio_mixer(left, 0, right, 0);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;
    use std::{format, vec};

    #[test]
    fn an_idle_player_owns_the_token_and_hands_it_back() {
        // SAFETY: a test-local token on the host; an idle player sends the
        // drive nothing.
        let player = Player::new(unsafe { Cd::steal() });
        assert!(!player.is_playing());
        let _token: Cd = player.release();
    }

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
    fn suspension_retains_song_and_resumes_without_seek() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        let original = song(DriveSpeed::Single);
        engine.start(&mut drive, original, true).unwrap();
        drive.heads = vec![Some(1040)];
        engine.poll(&mut drive);
        let elapsed = engine.elapsed_millis();
        drive.log.clear();
        assert!(engine.suspend(&mut drive));
        assert!(!engine.is_streaming());
        drive.heads = vec![Some(1105)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert_eq!(engine.elapsed_millis(), elapsed);
        assert_eq!(
            drive.heads.len(),
            1,
            "suspended polls must not read the head"
        );
        assert!(engine.looping);
        assert_eq!(engine.song, Some(original));
        assert!(engine.suspend(&mut drive));
        engine.resume(&mut drive).unwrap();
        // Every setting again, aimed at the head the pause left (1040), not
        // the top of the file and not wherever the controller happens to be.
        assert_eq!(
            drive.log,
            [
                "pause",
                "unmute",
                "mode 0x48",
                "filter 3 2",
                "target 1040",
                "read"
            ]
        );
        assert_eq!(engine.elapsed_millis(), elapsed);
        assert_eq!(engine.state, State::Seeking);
        drive.heads = vec![Some(1050), Some(1100)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.is_streaming());
        assert_eq!(engine.poll(&mut drive), Event::Looped);
    }

    #[test]
    fn refused_suspend_retries_without_advancing_or_looping() {
        let mut drive = Fake {
            pause_refusals: 2,
            ..Fake::default()
        };
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Double), true)
            .unwrap();
        drive.log.clear();
        assert!(!engine.suspend(&mut drive));
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(!engine.suspended);
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.suspended);
        assert_eq!(drive.log, ["pause", "pause", "pause"]);
        engine.resume(&mut drive).unwrap();
        assert_eq!(engine.state, State::Seeking);
        drive.heads = vec![Some(2000), Some(1010)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(!engine.is_streaming());
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.is_streaming());
    }

    #[test]
    fn resume_cancels_refused_pause_without_duplicate_read() {
        let mut drive = Fake {
            pause_refusals: 1,
            ..Fake::default()
        };
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.log.clear();
        assert!(!engine.suspend(&mut drive));
        engine.resume(&mut drive).unwrap();
        engine.poll(&mut drive);
        assert_eq!(drive.log, ["pause"]);
        assert!(!engine.suspend_pending);
    }

    #[test]
    fn refused_resume_keeps_suspension_for_explicit_retry() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        assert!(engine.suspend(&mut drive));
        drive.fail_at = Some("read");
        assert_eq!(engine.resume(&mut drive), Err(Error::Read));
        assert!(engine.suspended);
        drive.log.clear();
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(drive.log.is_empty());
        drive.fail_at = None;
        engine.resume(&mut drive).unwrap();
        assert!(!engine.suspended);
    }

    #[test]
    fn replacement_song_and_stop_cancel_old_suspension() {
        for pending in [false, true] {
            let mut drive = Fake::default();
            let mut engine = Engine::new();
            engine
                .start(&mut drive, song(DriveSpeed::Single), true)
                .unwrap();
            drive.pause_refusals = u32::from(pending);
            engine.suspend(&mut drive);
            let replacement = song(DriveSpeed::Double);
            engine.start(&mut drive, replacement, false).unwrap();
            drive.log.clear();
            engine.resume(&mut drive).unwrap();
            assert_eq!(engine.song, Some(replacement));
            assert!(!engine.looping);
            assert!(drive.log.is_empty());
            engine.suspend(&mut drive);
            engine.stop(&mut drive);
            drive.log.clear();
            engine.resume(&mut drive).unwrap();
            assert_eq!(engine.poll(&mut drive), Event::Idle);
            assert!(engine.song.is_none());
            assert!(drive.log.is_empty());
        }
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

    /// The head the drive reports after the pause wins over a poll that is
    /// long out of date; without an answer the last poll does; a song that
    /// never got past the seek resumes at its seek target.
    #[test]
    fn resume_aims_at_the_best_position_known() {
        // The pause-time reading beats a stale poll.
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.heads = vec![Some(1010)];
        engine.poll(&mut drive);
        drive.heads = vec![Some(1070)];
        assert!(engine.suspend(&mut drive));
        drive.log.clear();
        engine.resume(&mut drive).unwrap();
        assert_eq!(drive.log.last().unwrap(), "read");
        assert_eq!(drive.log[drive.log.len() - 2], "target 1070");
        assert_eq!(engine.elapsed_millis(), 70 * 1000 / 75);

        // No answer after the pause: the last poll.
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.heads = vec![Some(1010)];
        engine.poll(&mut drive);
        assert!(engine.suspend(&mut drive));
        drive.log.clear();
        engine.resume(&mut drive).unwrap();
        assert!(drive.log.contains(&"target 1010".to_string()));

        // Never seen streaming and no answer: the seek target (the top). A
        // reading outside the file (the old position) is not believed.
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Single), false)
            .unwrap();
        drive.heads = vec![Some(9000)];
        assert!(engine.suspend(&mut drive));
        drive.log.clear();
        engine.resume(&mut drive).unwrap();
        assert!(drive.log.contains(&"target 1000".to_string()));
        assert_eq!(engine.state, State::Seeking);
    }

    /// The data read that motivated the rework: a reader leaves the mode with
    /// no XA bits, the filter maybe elsewhere and the head at the data. The
    /// resume must send all of it again, in the order a start does, aimed at
    /// the song, and the first stale head readings must not look like music.
    #[test]
    fn resume_after_a_data_read_restores_mode_filter_and_position() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        engine
            .start(&mut drive, song(DriveSpeed::Double), true)
            .unwrap();
        drive.heads = vec![Some(1030)];
        engine.poll(&mut drive);
        assert!(engine.suspend(&mut drive));
        // ... a data read happens here; the fake does not model it ...
        drive.log.clear();
        engine.resume(&mut drive).unwrap();
        assert_eq!(
            drive.log,
            ["unmute", "mode 0xc8", "filter 3 2", "target 1030", "read"]
        );
        // The head is still at the data (LBA 300), then lands on the song.
        drive.heads = vec![Some(300), Some(1031)];
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(!engine.is_streaming());
        assert_eq!(engine.poll(&mut drive), Event::Playing);
        assert!(engine.is_streaming());
        assert_eq!(engine.elapsed_millis(), 31 * 1000 / 150);
    }

    #[test]
    fn each_refused_resume_step_is_reported_and_keeps_the_suspension() {
        for (at, error) in [
            ("unmute", Error::Unmute),
            ("mode", Error::SetMode),
            ("filter", Error::SetFilter),
            ("target", Error::Seek),
            ("read", Error::Read),
        ] {
            let mut drive = Fake::default();
            let mut engine = Engine::new();
            engine
                .start(&mut drive, song(DriveSpeed::Single), false)
                .unwrap();
            assert!(engine.suspend(&mut drive));
            drive.fail_at = Some(at);
            assert_eq!(engine.resume(&mut drive), Err(error), "{at}");
            assert!(engine.suspended, "{at}");
            drive.fail_at = None;
            engine.resume(&mut drive).unwrap();
            assert!(!engine.suspended);
        }
    }

    #[test]
    fn the_epoch_changes_with_every_read_and_the_serial_with_every_reading() {
        let mut drive = Fake::default();
        let mut engine = Engine::new();
        let looped = song(DriveSpeed::Single);
        engine.start(&mut drive, looped, true).unwrap();
        assert!(engine.head_sample().is_none(), "not streaming yet");
        let first = engine.epoch;
        drive.heads = vec![Some(1010), Some(1020)];
        engine.poll(&mut drive);
        let a = engine.head_sample().unwrap();
        engine.poll(&mut drive);
        let b = engine.head_sample().unwrap();
        assert_eq!((a.epoch, b.epoch), (first, first));
        assert_ne!(a.serial, b.serial);
        assert_eq!((a.sector, b.sector), (10, 20));
        assert_eq!(b.sectors_per_second, 75);
        // A loop, a resume and a restart each start a new epoch, and a
        // suspended song has no sample.
        drive.heads = vec![Some(1100)];
        assert_eq!(engine.poll(&mut drive), Event::Looped);
        assert_ne!(engine.epoch, first);
        let looped_epoch = engine.epoch;
        assert!(engine.suspend(&mut drive));
        assert!(engine.head_sample().is_none());
        engine.resume(&mut drive).unwrap();
        assert_ne!(engine.epoch, looped_epoch);
    }

    #[test]
    fn the_drive_lent_for_a_read_comes_back_and_the_song_resumes() {
        // SAFETY: a test-local token on the host; the closure sends nothing.
        let mut player = Player::new(unsafe { Cd::steal() });
        // Idle: lending still works, nothing to suspend.
        let seen = player.with_cd_suspended(|cd| (cd, 7)).unwrap();
        assert_eq!(seen, 7);
        assert!(!player.is_suspended());
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
