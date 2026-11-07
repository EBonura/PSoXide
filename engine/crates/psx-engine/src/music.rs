//! Cue-driven XA music for the cooked Music nodes.
//!
//! A Music node names a track number and a volume; [`MusicPlayer`] turns the
//! request into drive work. The songs are XA files on the disc (the cook's
//! `MUSIC*.XA`), listed in the level's [`LevelMusicTrack`] table: the node's
//! track `n` is entry `n - MUSIC_FIRST_TRACK`, the numbering CD-DA gave cooked
//! projects, so authored nodes keep their meaning.
//!
//! The drive decodes the audio itself, so the CPU's whole share is the start
//! (five commands), one position read every so often, and the volume ramp,
//! which is a write to the SPU's CD volume and never a drive command.
//!
//! # The drive is busy while a song plays
//!
//! Music is a read of the song's file, so no data can be read meanwhile. The
//! player shares the drive with data readers through [`crate::cd_drive`]: it
//! asks for the lease before it starts, never pre-empts a read, and when a
//! reader takes the drive (`yield_music_for_data`) the song is paused where the
//! head was and restarted from that point once the drive is free.
//!
//! # Loops
//!
//! A looping song is restarted by the drive when the head passes the end of
//! the song's span: a seek back to its first sector. The seek is the loop's
//! gap. The end is noticed by polling, which is sparse mid-song and every tick
//! near the end, so the detection adds at most a tick to the seek.

use crate::cd_drive::{self, Poll, SongOffset, SECTORS_PER_SECOND};
use psx_level::{LevelMusicTrack, MUSIC_FIRST_TRACK};

/// Volume of a Music node that binds none, as a percentage of full.
pub(crate) const DEFAULT_VOLUME_PERCENT: u8 = 25;
/// Ticks before a start the drive refused is tried again.
const RETRY_TICKS: u32 = 60;
/// Ticks between asks for the audio lease while a data read still holds the
/// drive. An ask is one cheap check, so it stays short whatever a console's
/// handoff costs.
const LEASE_RETRY_TICKS: u32 = 2;
/// Ticks from a start to the first position read. The seek of a start lands
/// within a few tenths of a second on an emulated drive, more on a console.
const FIRST_POLL_TICKS: u32 = 6;
/// Longest gap between position reads while a song plays. Each read is one
/// polled drive command, so mid-song it stays rare; as the head nears the end
/// of the song the next read is scheduled for the tick the head gets there.
const POLL_MAX_TICKS: u32 = 60;
/// A new fight starts its combat song up to this many seconds in.
const RANDOM_START_MAX_SECONDS: u32 = 15;
/// Ticks the engine budgets in per second.
const TICKS_PER_SECOND: u32 = 60;

/// Combat music ramps: one second in, two seconds out (60 Hz ticks).
///
/// Console handoffs between music and data reads take about a second each way
/// (`cd_drive::HANDOFF_VBLANKS`), not the emulator's few milliseconds: a combat
/// start that has to wait for a read, or a read that displaces the song, is
/// silent for about that long. The ramps are policy and stay as authored.
pub(crate) const COMBAT_MUSIC_FADE_IN_TICKS: u16 = 60;
pub(crate) const COMBAT_MUSIC_FADE_OUT_TICKS: u16 = 120;

/// What a Music node asks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct MusicCue {
    pub(crate) track: u8,
    pub(crate) volume_percent: u8,
    pub(crate) loop_track: bool,
}

impl MusicCue {
    pub(crate) const SILENT: Self = Self {
        track: 0,
        volume_percent: DEFAULT_VOLUME_PERCENT,
        loop_track: false,
    };

    pub(crate) const fn normalized(self) -> Self {
        Self {
            track: if self.track > 99 { 0 } else { self.track },
            volume_percent: if self.volume_percent > 100 {
                100
            } else {
                self.volume_percent
            },
            loop_track: self.loop_track,
        }
    }
}

/// The song a cue's track number names, if the cook wrote one.
fn song_for(tracks: &[LevelMusicTrack], track: u8) -> Option<&LevelMusicTrack> {
    tracks.get(usize::from(track.checked_sub(MUSIC_FIRST_TRACK)?))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct MusicPlayer {
    pub(crate) requested: MusicCue,
    current_track: u8,
    current_volume_percent: u8,
    target_volume_percent: u8,
    fade_start_volume_percent: u8,
    fade_ticks_total: u16,
    fade_ticks_left: u16,
    fade_out_stop: bool,
    random_start: bool,
    random_state: u32,
    next_retry_tick: u32,
    next_poll_tick: u32,
    routed: bool,
    /// Sectors into the song the current play began at. Non-zero for a random
    /// or resumed start, which plays to the end and then replays from the top.
    start_offset: SongOffset,
    /// The song ended and the drive was paused (a cue that does not loop).
    finished: bool,
    /// Where to restart a song a data read displaced, and which track it was.
    resume: Option<SongOffset>,
    resume_track: u8,
}

impl MusicPlayer {
    pub(crate) const fn new() -> Self {
        Self {
            requested: MusicCue::SILENT,
            current_track: 0,
            current_volume_percent: DEFAULT_VOLUME_PERCENT,
            target_volume_percent: DEFAULT_VOLUME_PERCENT,
            fade_start_volume_percent: DEFAULT_VOLUME_PERCENT,
            fade_ticks_total: 0,
            fade_ticks_left: 0,
            fade_out_stop: false,
            random_start: false,
            random_state: 0x6d2b79f5,
            next_retry_tick: 0,
            next_poll_tick: 0,
            routed: false,
            start_offset: 0,
            finished: false,
            resume: None,
            resume_track: 0,
        }
    }

    /// Start `cue` from silence and ramp to its volume over `fade_ticks`.
    pub(crate) fn start_faded(&mut self, cue: MusicCue, tick: u32, fade_ticks: u16) {
        let cue = cue.normalized();
        self.fade_out_stop = false;
        self.request(
            MusicCue {
                volume_percent: 0,
                ..cue
            },
            tick,
        );
        self.requested = cue;
        if self.current_track == 0 {
            self.random_start = true;
            self.random_state ^= tick;
            self.random_state = self.random_state.wrapping_mul(109).wrapping_add(1);
        }
        self.current_volume_percent = 0;
        self.target_volume_percent = cue.volume_percent;
        self.fade_start_volume_percent = 0;
        self.fade_ticks_total = fade_ticks.max(1);
        self.fade_ticks_left = fade_ticks.max(1);
        if self.routed {
            set_cd_volume(0);
        }
    }

    /// Ramp to silence over `fade_ticks`, then stop and release the drive.
    pub(crate) fn fade_out_and_stop(&mut self, fade_ticks: u16) {
        if self.requested.track == 0 {
            return;
        }
        self.target_volume_percent = 0;
        self.fade_start_volume_percent = self.current_volume_percent;
        self.fade_ticks_total = fade_ticks.max(1);
        self.fade_ticks_left = fade_ticks.max(1);
        self.fade_out_stop = true;
    }

    pub(crate) fn fading_out(&self) -> bool {
        self.fade_out_stop
    }

    /// One tick of the volume ramp; SPU CD volume is a register write.
    fn tick_fade(&mut self, tick: u32) {
        if self.fade_ticks_left == 0 {
            return;
        }
        self.fade_ticks_left -= 1;
        // Interpolate from the original level, retaining sub-percent progress
        // until the final division. Rounding each tick's step up shortens low
        // volume fades and makes even a full-volume ramp nonlinear.
        // Both levels are at most 100 and duration is u16, so i32 suffices.
        let elapsed = i32::from(self.fade_ticks_total - self.fade_ticks_left);
        let start = i32::from(self.fade_start_volume_percent);
        let delta = i32::from(self.target_volume_percent) - start;
        let next = (start + delta * elapsed / i32::from(self.fade_ticks_total)) as u8;
        if self.current_volume_percent != next {
            self.current_volume_percent = next;
            if self.routed {
                set_cd_volume(next);
            }
        }
        if self.fade_ticks_left == 0 && self.fade_out_stop {
            self.release_for_data_reads(tick);
        }
    }

    pub(crate) fn request(&mut self, cue: MusicCue, tick: u32) {
        let cue = cue.normalized();
        if self.requested == cue {
            return;
        }

        let track_changed = self.requested.track != cue.track;
        self.requested = cue;

        if cue.track == 0 {
            self.release_for_data_reads(tick);
            return;
        }

        self.target_volume_percent = cue.volume_percent;
        self.fade_ticks_left = 0;
        self.fade_out_stop = false;
        if self.routed && self.current_volume_percent != cue.volume_percent {
            set_cd_volume(cue.volume_percent);
            self.current_volume_percent = cue.volume_percent;
        }

        if track_changed || self.current_track != cue.track {
            self.current_track = 0;
            self.random_start = false;
            self.finished = false;
            self.next_retry_tick = tick;
        }
    }

    /// Stop the music on purpose so data reads can have the drive: Pause (never
    /// Stop), then give the audio lease back. A displaced song's pending
    /// resume is dropped with it.
    pub(crate) fn release_for_data_reads(&mut self, tick: u32) {
        self.resume = None;
        cd_drive::clear_yield();
        self.requested = MusicCue::SILENT;
        self.random_start = false;
        self.current_track = 0;
        self.finished = false;
        self.start_offset = 0;
        self.next_retry_tick = tick;
        self.routed = false;
        self.fade_ticks_left = 0;
        self.fade_out_stop = false;
        if cd_drive::music_holds() {
            cd_drive::stop_playback();
        }
        cd_drive::music_release();
    }

    /// One tick of music work against the level's song table.
    #[inline(never)]
    pub(crate) fn update(&mut self, tick: u32, tracks: &[LevelMusicTrack]) {
        if self.requested.track == 0 {
            return;
        }
        self.tick_fade(tick);
        if self.requested.track == 0 {
            return;
        }
        if self.routed && !cd_drive::music_holds() {
            self.displaced(tick);
        }
        if cd_drive::resume_pending() {
            // A data read displaced the song. Wait until the drive is free,
            // then start it again at the saved point.
            let Some(offset) = cd_drive::take_resume() else {
                return;
            };
            self.resume = offset;
        }
        // A cue naming a song the cook did not write stays silent and never
        // takes the drive.
        let Some(song) = song_for(tracks, self.requested.track) else {
            return;
        };
        if self.current_track == self.requested.track {
            self.follow(tick, song);
            return;
        }
        if tick < self.next_retry_tick {
            return;
        }
        self.start(tick, song);
    }

    /// Take the drive if it is free, route the audio and start the song.
    fn start(&mut self, tick: u32, song: &LevelMusicTrack) {
        if !self.routed {
            // Music asks for the drive; a data read in flight or queued keeps
            // it, and the lease is granted a tick or two after the last read.
            if !cd_drive::music_acquire() {
                self.next_retry_tick = tick.saturating_add(LEASE_RETRY_TICKS);
                return;
            }
            // A faded start routes at the ramp's current level (0), not the cue's.
            if self.fade_ticks_left == 0 {
                self.current_volume_percent = self.requested.volume_percent;
            }
            route_audio(self.current_volume_percent);
            self.routed = true;
        }
        // A displaced song comes back where it was; a new fight starts a little
        // way in; anything else starts at the top.
        let resumed = self
            .resume
            .filter(|_| self.resume_track == self.requested.track);
        let random = if self.random_start {
            random_start_offset(song.span_sectors, self.random_state)
        } else {
            None
        };
        let offset = resumed.or(random).unwrap_or(0);
        // A song that starts part way in is not left to loop on its own: the
        // drive would loop it back to that point, so it plays to its end and
        // the player replays it from the top.
        let looping = self.requested.loop_track && offset == 0;
        if cd_drive::music_play(song, offset, looping) {
            self.random_start = false;
            self.resume = None;
            self.finished = false;
            self.start_offset = offset;
            self.current_track = self.requested.track;
            self.next_poll_tick = tick.saturating_add(FIRST_POLL_TICKS);
        } else {
            self.next_retry_tick = tick.saturating_add(RETRY_TICKS);
        }
    }

    /// A data read took the drive: the song was paused where the head stood
    /// and the lease released. Forget the playback state so the start runs
    /// again once the drive is free; the saved point replaces the random
    /// start.
    fn displaced(&mut self, tick: u32) {
        self.resume_track = self.current_track;
        self.random_start = false;
        self.current_track = 0;
        self.next_retry_tick = tick;
        self.routed = false;
    }

    /// Notice the end of a playing song, and when to look next.
    fn follow(&mut self, tick: u32, song: &LevelMusicTrack) {
        if self.finished || tick < self.next_poll_tick {
            return;
        }
        match cd_drive::music_poll() {
            Poll::Looped => self.next_poll_tick = tick.saturating_add(FIRST_POLL_TICKS),
            Poll::Finished if self.requested.loop_track && self.start_offset != 0 => {
                // The part-way start reached the end: a loop belongs at the top
                // of the song.
                if cd_drive::music_play(song, 0, true) {
                    self.start_offset = 0;
                    self.next_poll_tick = tick.saturating_add(FIRST_POLL_TICKS);
                } else {
                    self.current_track = 0;
                    self.next_retry_tick = tick.saturating_add(RETRY_TICKS);
                }
            }
            Poll::Finished => self.finished = true,
            Poll::Idle => {
                // The drive lost the song: start it again.
                self.current_track = 0;
                self.next_retry_tick = tick;
            }
            Poll::Playing => {
                let wait = match cd_drive::music_head() {
                    Some(head) => {
                        let remaining = song.span_sectors.saturating_sub(head);
                        (remaining * TICKS_PER_SECOND / SECTORS_PER_SECOND).clamp(1, POLL_MAX_TICKS)
                    }
                    None => FIRST_POLL_TICKS,
                };
                self.next_poll_tick = tick.saturating_add(wait);
            }
        }
    }
}

/// Where a new fight's combat song begins, in sectors from the song's start.
///
/// Whole seconds, one second in at the earliest and four before the end at the
/// latest, and within the opening [`RANDOM_START_MAX_SECONDS`]: new fights vary
/// only in the opening of the song, so they cannot begin near the end of a
/// long song and immediately incur the loop's return seek.
fn random_start_offset(span_sectors: u32, seed: u32) -> Option<SongOffset> {
    let span = (span_sectors / SECTORS_PER_SECOND)
        .checked_sub(4)?
        .min(RANDOM_START_MAX_SECONDS);
    if span == 0 {
        return None;
    }
    Some((1 + seed % span) * SECTORS_PER_SECOND)
}

#[cfg(target_arch = "mips")]
fn route_audio(volume_percent: u8) {
    set_cd_volume(volume_percent);
    psx_spu::enable_cd_audio(true);
}

#[cfg(not(target_arch = "mips"))]
fn route_audio(_volume_percent: u8) {}

#[cfg(target_arch = "mips")]
fn set_cd_volume(volume_percent: u8) {
    let volume = psx_spu::CdVolume::linear(volume_percent.min(100) as u16, 100);
    psx_spu::set_cd_volume(volume, volume);
}

#[cfg(not(target_arch = "mips"))]
fn set_cd_volume(_volume_percent: u8) {}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::cd_drive::{
        host_events, host_script_polls, host_set_data_busy, host_set_play_fails, host_set_position,
        yield_music_for_data, HostEvent,
    };
    use std::vec;
    use std::vec::Vec;

    /// Song of 40 seconds at sector 1000 of file 1, channel 0; and a second
    /// one of 20 seconds on channel 1 of the same file.
    const TRACKS: &[LevelMusicTrack] = &[
        LevelMusicTrack {
            file_lba: 1000,
            span_sectors: 3000,
            file_number: 1,
            channel: 0,
        },
        LevelMusicTrack {
            file_lba: 1000,
            span_sectors: 1500,
            file_number: 1,
            channel: 1,
        },
    ];

    fn cue(track: u8, volume: u8, looping: bool) -> MusicCue {
        MusicCue {
            track,
            volume_percent: volume,
            loop_track: looping,
        }
    }

    fn steady() -> MusicCue {
        cue(2, 80, false)
    }

    fn plays() -> Vec<HostEvent> {
        host_events()
            .into_iter()
            .filter(|e| matches!(e, HostEvent::Play { .. }))
            .collect()
    }

    fn run(player: &mut MusicPlayer, from: u32, ticks: u32) {
        for tick in from..from + ticks {
            player.update(tick, TRACKS);
        }
    }

    #[test]
    fn a_cue_plays_the_song_its_track_number_names() {
        let mut player = MusicPlayer::new();
        player.request(cue(3, 80, true), 0);
        run(&mut player, 0, 10);
        assert_eq!(
            plays(),
            vec![HostEvent::Play {
                lba: 1000,
                span: 1500,
                offset: 0,
                looping: true,
            }]
        );
        assert_eq!(player.current_track, 3);
    }

    #[test]
    fn a_track_without_a_cooked_song_stays_silent_and_leaves_the_drive_alone() {
        for track in [1, 4, 99] {
            let mut player = MusicPlayer::new();
            player.request(cue(track, 80, true), 0);
            run(&mut player, 0, 60);
            assert!(host_events().is_empty(), "track {track} took the drive");
            assert_eq!(player.current_track, 0);
        }
    }

    #[test]
    fn music_takes_the_audio_lease_before_it_starts_the_song() {
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 20);
        assert!(matches!(
            host_events().as_slice(),
            [HostEvent::Acquire, HostEvent::Play { .. }]
        ));
        assert_eq!(player.current_track, 2);
        player.release_for_data_reads(30);
        assert_eq!(host_events().last(), Some(&HostEvent::Release));
    }

    #[test]
    fn music_waits_for_a_data_read_instead_of_pre_empting_it() {
        host_set_data_busy(true);
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 40);
        assert!(host_events().is_empty(), "no lease while a read is queued");
        host_set_data_busy(false);
        run(&mut player, 40, 20);
        assert_eq!(player.current_track, 2);
        assert_eq!(plays().len(), 1);
    }

    #[test]
    fn a_refused_start_is_tried_again_later() {
        host_set_play_fails(true);
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, RETRY_TICKS - 1);
        assert_eq!(plays().len(), 1, "one try, then it waits");
        assert_eq!(player.current_track, 0);
        host_set_play_fails(false);
        run(&mut player, RETRY_TICKS - 1, 2);
        assert_eq!(plays().len(), 2);
        assert_eq!(player.current_track, 2);
    }

    #[test]
    fn a_displaced_song_resumes_where_the_head_was() {
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 20);
        assert_eq!(player.current_track, 2);

        // A data read needs the drive: the song is paused 700 sectors in.
        host_set_position(Some(700));
        assert!(yield_music_for_data());
        host_set_data_busy(true);
        assert!(matches!(
            host_events().as_slice(),
            [
                HostEvent::Acquire,
                HostEvent::Play { .. },
                HostEvent::Release,
                HostEvent::Yield
            ]
        ));
        // While the read runs the player stays quiet and keeps its cue.
        let before = host_events().len();
        run(&mut player, 100, 30);
        assert_eq!(player.requested.track, 2);
        assert_eq!(
            host_events().len(),
            before,
            "music did not take the drive back"
        );

        // The read is done: the lease comes back and the song restarts from
        // the saved point, not from the beginning and not at a random point.
        host_set_data_busy(false);
        run(&mut player, 130, 30);
        assert_eq!(
            plays().last(),
            Some(&HostEvent::Play {
                lba: 1000,
                span: 3000,
                offset: 700,
                looping: false,
            })
        );
        assert_eq!(player.current_track, 2);
        assert_eq!(player.resume, None);
    }

    #[test]
    fn a_resume_belongs_to_the_song_that_was_displaced() {
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 20);
        host_set_position(Some(700));
        assert!(yield_music_for_data());
        host_set_data_busy(true);
        run(&mut player, 100, 5);
        // The scene asks for another song while the read runs.
        player.request(cue(3, 80, false), 110);
        host_set_data_busy(false);
        run(&mut player, 110, 30);
        assert_eq!(
            plays().last(),
            Some(&HostEvent::Play {
                lba: 1000,
                span: 1500,
                offset: 0,
                looping: false,
            })
        );
    }

    #[test]
    fn stopping_music_on_purpose_does_not_resume_it() {
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 20);
        host_set_position(Some(700));
        assert!(yield_music_for_data());
        host_set_data_busy(true);
        // The scene ends the music while it is displaced.
        player.release_for_data_reads(50);
        host_set_data_busy(false);
        let before = host_events().len();
        run(&mut player, 60, 40);
        assert_eq!(host_events().len(), before);
        assert_eq!(player.current_track, 0);
    }

    #[test]
    fn a_new_fight_randomizes_but_a_running_song_does_not_reseek() {
        let cue = cue(2, 80, true);
        let mut player = MusicPlayer::new();
        player.request(cue, 0);
        assert!(!player.random_start, "ordinary menu music starts normally");
        player.start_faded(cue, 100, 60);
        assert!(player.random_start);
        let first_seed = player.random_state;
        player.current_track = cue.track;
        player.random_start = false;
        player.fade_out_and_stop(120);
        player.start_faded(cue, 130, 60);
        assert!(
            !player.random_start,
            "re-engaging before fade-out finishes keeps playback"
        );
        assert_eq!(player.random_state, first_seed);
        player.release_for_data_reads(200);
        player.start_faded(cue, 500, 60);
        assert!(player.random_start);
        assert_ne!(player.random_state, first_seed);
        assert!(player.requested.loop_track);
    }

    #[test]
    fn a_random_start_begins_inside_the_opening_and_replays_from_the_top() {
        let looping = cue(2, 80, true);
        let mut player = MusicPlayer::new();
        player.start_faded(looping, 100, 60);
        run(&mut player, 101, 10);
        let Some(HostEvent::Play { offset, .. }) = plays().last().copied() else {
            panic!("no song started");
        };
        assert!(
            (75..=15 * 75).contains(&offset) && offset % 75 == 0,
            "{offset}"
        );
        assert_eq!(player.start_offset, offset);
        let seed = player.random_state;

        // The part-way start is not left to loop by itself; it plays to the end
        // and the player replays it from the top, with no new random.
        let all = plays();
        assert!(matches!(all[0], HostEvent::Play { looping: false, .. }));
        host_script_polls(&[Poll::Finished]);
        run(&mut player, 200, 100);
        let all = plays();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all[1],
            HostEvent::Play {
                lba: 1000,
                span: 3000,
                offset: 0,
                looping: true,
            }
        );
        assert_eq!(player.start_offset, 0);
        assert_eq!(player.random_state, seed);
        // A later loop of the top-started song needs no replay.
        host_script_polls(&[Poll::Looped]);
        run(&mut player, 300, 100);
        assert_eq!(plays().len(), 2);
    }

    #[test]
    fn random_music_start_stays_inside_the_song() {
        for seed in [0, 12345, 987654, u32::MAX] {
            let offset = random_start_offset(200 * 75, seed).unwrap();
            assert!((75..=15 * 75).contains(&offset));
        }
        assert_eq!(random_start_offset(4 * 75, 123), None);
        assert_eq!(random_start_offset(3 * 75, 123), None);
        assert_eq!(random_start_offset(0, 123), None);
        // Short songs keep the end margin.
        for seed in 0..1024 {
            let offset = random_start_offset(10 * 75, seed).unwrap();
            assert!((75..=6 * 75).contains(&offset), "{offset}");
        }
        let seconds: std::collections::BTreeSet<_> = (0..100)
            .map(|seed| random_start_offset(200 * 75, seed).unwrap() / 75)
            .collect();
        assert_eq!(seconds, (1..=15).collect());
    }

    #[test]
    fn polls_are_rare_mid_song_and_frequent_at_the_end() {
        let mut player = MusicPlayer::new();
        player.request(cue(2, 80, true), 0);
        run(&mut player, 0, 2);
        assert_eq!(player.current_track, 2);
        // Mid-song (head at sector 100 of 3000): the next look is a long way
        // off, bounded by POLL_MAX_TICKS.
        host_set_position(Some(100));
        player.next_poll_tick = 0;
        player.update(10, TRACKS);
        assert_eq!(player.next_poll_tick, 10 + POLL_MAX_TICKS);
        // 45 sectors (0.6 s, 36 ticks) from the end: look when the head gets
        // there.
        host_set_position(Some(3000 - 45));
        let at = player.next_poll_tick;
        player.update(at, TRACKS);
        assert_eq!(player.next_poll_tick, at + 36);
        // At the end the next look is the next tick.
        host_set_position(Some(3000));
        let at = player.next_poll_tick;
        player.update(at, TRACKS);
        assert_eq!(player.next_poll_tick, at + 1);
    }

    #[test]
    fn a_loop_is_noticed_within_a_tick_of_the_head_reaching_the_end() {
        let mut player = MusicPlayer::new();
        player.request(cue(2, 80, true), 0);
        run(&mut player, 0, 2);
        // The head advances 75 sectors a second, 1.25 a tick, and crosses the
        // end of the 3000-sector song at tick END; the drive loops it then.
        const END: u32 = 2 + 2400;
        let mut noticed = None;
        for tick in 2..END + 100 {
            host_set_position(Some(((tick - 2) * 5 / 4).min(3000)));
            if tick == END {
                host_script_polls(&[Poll::Looped]);
            }
            let before = player.next_poll_tick;
            player.update(tick, TRACKS);
            if tick >= END && noticed.is_none() && player.next_poll_tick != before {
                noticed = Some(tick);
            }
        }
        let late = noticed.expect("the loop was never noticed") - END;
        assert!(late <= 1, "noticed {late} ticks after the end");
    }

    #[test]
    fn a_song_that_does_not_loop_stops_polling_when_it_ends() {
        let mut player = MusicPlayer::new();
        player.request(steady(), 0);
        run(&mut player, 0, 2);
        host_script_polls(&[Poll::Finished]);
        run(&mut player, 10, 100);
        assert!(player.finished);
        assert_eq!(player.current_track, 2, "the cue is still the song's");
        // Nothing restarts it until the cue changes.
        run(&mut player, 200, 100);
        assert_eq!(plays().len(), 1);
        player.request(cue(3, 80, false), 400);
        run(&mut player, 400, 10);
        assert_eq!(plays().len(), 2);
    }

    #[test]
    fn a_song_the_drive_lost_is_started_again() {
        let mut player = MusicPlayer::new();
        player.request(cue(2, 80, true), 0);
        run(&mut player, 0, 2);
        host_script_polls(&[Poll::Idle]);
        run(&mut player, 10, 20);
        assert_eq!(plays().len(), 2);
        assert_eq!(player.current_track, 2);
    }

    #[test]
    fn a_track_change_restarts_with_the_new_song() {
        let mut player = MusicPlayer::new();
        player.request(cue(2, 80, true), 0);
        run(&mut player, 0, 10);
        player.request(cue(3, 80, true), 20);
        run(&mut player, 20, 10);
        let all = plays();
        assert_eq!(all.len(), 2);
        assert!(matches!(all[1], HostEvent::Play { span: 1500, .. }));
        assert_eq!(player.current_track, 3);
    }

    #[test]
    fn cd_data_read_handoff_cancels_a_pending_fade_stop() {
        let mut player = MusicPlayer::new();
        player.request(cue(2, 80, true), 0);
        player.fade_out_and_stop(120);
        player.release_for_data_reads(1);
        assert!(
            !player.fading_out(),
            "a loading transition must not suppress later menu music"
        );
    }

    #[test]
    fn fades_keep_their_duration_at_every_music_volume() {
        for volume in [1, 40, 80, 100] {
            let mut player = MusicPlayer::new();
            let cue = cue(2, volume, true);
            player.start_faded(cue, 0, 60);
            for tick in 1..60 {
                player.tick_fade(tick);
                assert!(
                    player.current_volume_percent < volume,
                    "fade-in ended early at volume {volume}"
                );
            }
            player.tick_fade(60);
            assert_eq!(player.current_volume_percent, volume);
            player.fade_out_and_stop(120);
            for tick in 61..180 {
                player.tick_fade(tick);
                assert!(
                    player.current_volume_percent > 0,
                    "fade-out ended early at volume {volume}"
                );
                assert_eq!(player.requested.track, 2);
            }
            player.tick_fade(180);
            assert_eq!(player.current_volume_percent, 0);
            assert_eq!(player.requested, MusicCue::SILENT);
            assert!(!player.fading_out());
        }
    }

    #[test]
    fn fades_are_linear_and_can_reverse_before_finishing() {
        let mut player = MusicPlayer::new();
        let cue = cue(2, 80, true);
        player.start_faded(cue, 0, 60);
        for tick in 1..=30 {
            player.tick_fade(tick);
        }
        assert_eq!(player.current_volume_percent, 40);
        player.fade_out_and_stop(120);
        for tick in 31..=90 {
            player.tick_fade(tick);
        }
        assert_eq!(player.current_volume_percent, 20);
        player.start_faded(cue, 90, 60);
        for tick in 91..=150 {
            player.tick_fade(tick);
        }
        assert_eq!(player.current_volume_percent, 80);
        assert_eq!(player.requested, cue);
        assert!(!player.fading_out());
    }

    #[test]
    fn a_combat_fade_runs_over_the_same_ticks_whatever_the_drive_does() {
        // The fade is a CD volume ramp and never waits on a drive command, so
        // its ticks are the authored 60 in and 120 out even when the start
        // is delayed by a data read.
        host_set_data_busy(true);
        let mut player = MusicPlayer::new();
        let combat = cue(3, 100, true);
        player.start_faded(combat, 0, COMBAT_MUSIC_FADE_IN_TICKS);
        let mut full_at = None;
        for tick in 1..=200 {
            player.update(tick, TRACKS);
            if full_at.is_none() && player.current_volume_percent == 100 {
                full_at = Some(tick);
            }
        }
        assert_eq!(full_at, Some(u32::from(COMBAT_MUSIC_FADE_IN_TICKS)));
        player.fade_out_and_stop(COMBAT_MUSIC_FADE_OUT_TICKS);
        let mut silent_at = None;
        for tick in 201..=400 {
            player.update(tick, TRACKS);
            if silent_at.is_none() && player.requested.track == 0 {
                silent_at = Some(tick - 200);
            }
        }
        assert_eq!(silent_at, Some(u32::from(COMBAT_MUSIC_FADE_OUT_TICKS)));
    }
}
