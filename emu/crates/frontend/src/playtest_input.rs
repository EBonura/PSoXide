//! Input recording + replay for the editor playtest, built on the shared
//! [`emulator_core::input_tape`] `PXITAPE1` format. The same tapes drive
//! headless commercial-game / homebrew regression runs (`cli.rs
//! --input-tape`) and CI -- recording happens at the bus port-1 boundary,
//! which is game-agnostic.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(target_arch = "wasm32", test))]
use emulator_core::input_tape::tape_to_csv;
use emulator_core::input_tape::{read_tape_full, write_tape_poll_bound, TapeClock};
#[cfg(feature = "editor")]
use psxed_ui::{EditorPlaytestTapeMode, EditorPlaytestTapeStatus};

/// One emulated frame's port-1 pad state. Alias to the shared tape sample
/// so existing frontend call sites stay stable while the type + binary
/// format live in the core crate.
pub(crate) use emulator_core::input_tape::PadSample as Port1PadSample;

/// Read a persisted input tape. Only the headless CLI (`--input-tape`) reads
/// tapes from a path; that CLI is compiled out on wasm, so this is dead there.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn read_input_tape(path: &Path) -> Result<emulator_core::input_tape::Tape, String> {
    read_tape_full(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PlaytestInputMode {
    #[default]
    Idle,
    Recording,
    Replaying,
}

/// One state transition emitted while applying a tape frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaytestInputEvent {
    /// Replay consumed the final recorded frame.
    ReplayFinished { frames: usize },
}

/// Mutable input tape state owned by the frontend app.
#[derive(Debug, Default)]
pub(crate) struct PlaytestInputTape {
    mode: PlaytestInputMode,
    samples: Vec<Port1PadSample>,
    replay_cursor: usize,
    /// Port-1 completed-poll count when recording started, persisted in the
    /// tape so a replay from a cold boot knows which poll sample 0 belongs to.
    start_poll: u64,
    /// Set when the loaded tape is video-frame clocked (`PXITAPE1`). Those
    /// tapes cannot be replayed deterministically across builds of differing
    /// speed, so they are advanced one sample per frame, as before.
    frame_clocked: bool,
    /// Poll-bound replay from a cold boot: guest polls still to elapse before
    /// sample 0 is due. While non-zero the replay feeds an idle pad, exactly
    /// like the headless CLI's pre-`start_poll` window. Zero for in-place
    /// replay (editor), where the session is already at the recording point.
    polls_before_start: u64,
}

impl PlaytestInputTape {
    /// Editor-facing summary for controls and overlays.
    #[cfg(feature = "editor")]
    pub(crate) fn editor_status(&self) -> EditorPlaytestTapeStatus {
        EditorPlaytestTapeStatus {
            mode: match self.mode {
                PlaytestInputMode::Idle => EditorPlaytestTapeMode::Idle,
                PlaytestInputMode::Recording => EditorPlaytestTapeMode::Recording,
                PlaytestInputMode::Replaying => EditorPlaytestTapeMode::Replaying,
            },
            frames: self.samples.len() as u32,
            cursor: self.replay_cursor.min(self.samples.len()) as u32,
        }
    }

    /// True while live input is being appended to a tape.
    pub(crate) fn is_recording(&self) -> bool {
        self.mode == PlaytestInputMode::Recording
    }

    /// True while saved input is replacing live port-1 samples.
    pub(crate) fn is_replaying(&self) -> bool {
        self.mode == PlaytestInputMode::Replaying
    }

    /// Number of video-frame samples currently retained.
    pub(crate) fn frame_count(&self) -> usize {
        self.samples.len()
    }

    /// Start a new tape, discarding the in-memory previous recording.
    /// `poll_count` is the guest's current port-1 completed-poll count.
    pub(crate) fn start_recording(&mut self, poll_count: u64) {
        self.samples.clear();
        self.replay_cursor = 0;
        self.start_poll = poll_count;
        self.frame_clocked = false;
        self.mode = PlaytestInputMode::Recording;
    }

    /// Advance the tape by the pad polls the guest completed during the frame
    /// that just ran. Recording appends the sample the guest actually read for
    /// each of them; replay steps its cursor by the same amount, so both sides
    /// are indexed by the guest's own input clock rather than by wall time.
    pub(crate) fn note_polls(&mut self, live_sample: Port1PadSample, polls: u64) {
        if polls == 0 {
            return;
        }
        match self.mode {
            PlaytestInputMode::Recording => {
                for _ in 0..polls {
                    self.samples.push(live_sample);
                }
            }
            PlaytestInputMode::Replaying if !self.frame_clocked => {
                // The pre-start window absorbs polls first; only the excess
                // advances the tape.
                let gated = self.polls_before_start.min(polls);
                self.polls_before_start -= gated;
                self.replay_cursor = self
                    .replay_cursor
                    .saturating_add((polls - gated) as usize)
                    .min(self.samples.len());
            }
            _ => {}
        }
    }

    /// Stop recording and persist the tape.
    pub(crate) fn stop_recording(&mut self, path: &Path) -> Result<usize, String> {
        let frames = self.finish_recording();
        archive_existing_tape(path)?;
        write_tape_poll_bound(path, &self.samples, self.start_poll)?;
        Ok(frames)
    }

    /// Stop recording without touching a host filesystem and return the CSV
    /// payload used by the browser download path. Poll-clocked (v2) so the
    /// tape replays on the guest's own input clock, plus the recorder's game
    /// hash so replay can flag a changed build.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn stop_recording_csv(&mut self, game_hash: Option<u64>) -> (usize, String) {
        let frames = self.finish_recording();
        (
            frames,
            tape_to_csv(&self.samples, TapeClock::PadPoll, self.start_poll, game_hash),
        )
    }

    /// Start replaying a persisted tape, falling back to memory. In-place
    /// replay: the session is assumed to already be at the recording point,
    /// so no pre-`start_poll` idle window applies.
    pub(crate) fn start_replay(&mut self, path: &Path) -> Result<usize, String> {
        if path.is_file() {
            let tape = read_tape_full(path)?;
            self.samples = tape.samples;
            self.start_poll = tape.start_poll;
            self.frame_clocked = tape.clock == TapeClock::VideoFrame;
        }
        if self.samples.is_empty() {
            return Err("no recorded input tape found".to_string());
        }
        self.polls_before_start = 0;
        self.replay_cursor = 0;
        self.mode = PlaytestInputMode::Replaying;
        Ok(self.samples.len())
    }

    /// Start replaying an in-memory tape against a machine that was just
    /// cold-booted (the browser upload path). Poll-bound tapes feed an idle
    /// pad until the guest completes `start_poll` polls, mirroring the
    /// headless CLI's cold-boot alignment.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn start_replay_from_tape(
        &mut self,
        tape: emulator_core::input_tape::Tape,
    ) -> Result<usize, String> {
        if tape.samples.is_empty() {
            return Err("input tape has no frames".to_string());
        }
        self.frame_clocked = tape.clock == TapeClock::VideoFrame;
        self.start_poll = tape.start_poll;
        self.polls_before_start = if self.frame_clocked {
            0
        } else {
            tape.start_poll
        };
        self.samples = tape.samples;
        self.replay_cursor = 0;
        self.mode = PlaytestInputMode::Replaying;
        Ok(self.samples.len())
    }

    /// Stop replaying without discarding the loaded tape.
    pub(crate) fn stop_replay(&mut self) {
        if self.mode == PlaytestInputMode::Replaying {
            self.mode = PlaytestInputMode::Idle;
        }
    }

    /// Return the sample to apply for this emulated frame.
    pub(crate) fn sample_for_frame(
        &mut self,
        live_sample: Port1PadSample,
    ) -> (Port1PadSample, Option<PlaytestInputEvent>) {
        match self.mode {
            PlaytestInputMode::Idle => (live_sample, None),
            PlaytestInputMode::Recording => (live_sample, None),
            PlaytestInputMode::Replaying if !self.frame_clocked => {
                // Cold-boot alignment: until the guest reaches the tape's
                // start poll, feed a released pad (the same nothing the CLI
                // applies before `start_poll`), not the user's live input.
                if self.polls_before_start > 0 {
                    return (Port1PadSample::from_buttons(0), None);
                }
                let Some(sample) = self.samples.get(self.replay_cursor).copied() else {
                    let frames = self.samples.len();
                    self.mode = PlaytestInputMode::Idle;
                    return (
                        live_sample,
                        Some(PlaytestInputEvent::ReplayFinished { frames }),
                    );
                };
                (sample, None)
            }
            PlaytestInputMode::Replaying => {
                let Some(sample) = self.samples.get(self.replay_cursor).copied() else {
                    let frames = self.samples.len();
                    self.mode = PlaytestInputMode::Idle;
                    return (
                        live_sample,
                        Some(PlaytestInputEvent::ReplayFinished { frames }),
                    );
                };
                self.replay_cursor += 1;
                let event = if self.replay_cursor == self.samples.len() {
                    self.mode = PlaytestInputMode::Idle;
                    Some(PlaytestInputEvent::ReplayFinished {
                        frames: self.samples.len(),
                    })
                } else {
                    None
                };
                (sample, event)
            }
        }
    }

    fn finish_recording(&mut self) -> usize {
        let frames = self.samples.len();
        if self.mode == PlaytestInputMode::Recording {
            self.mode = PlaytestInputMode::Idle;
        }
        frames
    }
}

/// Preserve the previous `latest.pxtape` before replacing it. Recordings can
/// represent hours of navigation and must not silently disappear when a new
/// capture starts.
fn archive_existing_tape(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("input tape has no parent directory: {}", path.display()))?;
    let archive_dir = parent.join("archive");
    std::fs::create_dir_all(&archive_dir)
        .map_err(|error| format!("create {}: {error}", archive_dir.display()))?;
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_| SystemTime::now());
    let elapsed = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let base = format!(
        "recording-{}-{:09}",
        elapsed.as_secs(),
        elapsed.subsec_nanos()
    );
    let mut archive_path = archive_dir.join(format!("{base}.pxtape"));
    let mut suffix = 1u32;
    while archive_path.exists() {
        archive_path = archive_dir.join(format!("{base}-{suffix}.pxtape"));
        suffix = suffix.saturating_add(1);
    }
    std::fs::copy(path, &archive_path).map_err(|error| {
        format!(
            "archive {} to {}: {error}",
            path.display(),
            archive_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_emulator_recording_round_trips_through_shared_format() {
        let path = std::env::temp_dir().join(format!(
            "psoxide-input-tape-test-{}.pxtape",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let frames = [
            Port1PadSample {
                buttons: 0xffef,
                right_x: 0x80,
                right_y: 0x80,
                left_x: 0x80,
                left_y: 0x80,
            },
            Port1PadSample {
                buttons: 0xffff,
                right_x: 0x70,
                right_y: 0x90,
                left_x: 0x60,
                left_y: 0xa0,
            },
        ];
        let mut tape = PlaytestInputTape::default();
        tape.start_recording(0);
        for frame in frames {
            assert_eq!(tape.sample_for_frame(frame).0, frame);
            tape.note_polls(frame, 1);
        }
        assert_eq!(tape.stop_recording(&path).unwrap(), frames.len());
        assert_eq!(read_input_tape(&path).unwrap().samples, frames);

        let mut replay = PlaytestInputTape::default();
        assert_eq!(replay.start_replay(&path).unwrap(), frames.len());
        assert_eq!(
            replay.sample_for_frame(Port1PadSample::default()).0,
            frames[0]
        );
        // A poll-bound replay holds sample 0 until the guest polls, however
        // many host frames that takes.
        assert_eq!(
            replay.sample_for_frame(Port1PadSample::default()).0,
            frames[0]
        );
        replay.note_polls(Port1PadSample::default(), 1);
        let (last, _event) = replay.sample_for_frame(Port1PadSample::default());
        assert_eq!(last, frames[1]);
        replay.note_polls(Port1PadSample::default(), 1);
        let (_, event) = replay.sample_for_frame(Port1PadSample::default());
        assert_eq!(
            event,
            Some(PlaytestInputEvent::ReplayFinished {
                frames: frames.len()
            })
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn replacing_latest_tape_archives_the_previous_recording() {
        let root = std::env::temp_dir().join(format!(
            "psoxide-input-tape-archive-test-{}",
            std::process::id()
        ));
        let path = root.join("latest.pxtape");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let first = Port1PadSample {
            buttons: 0xfffe,
            ..Port1PadSample::default()
        };
        let second = Port1PadSample {
            buttons: 0xfffd,
            ..Port1PadSample::default()
        };
        let mut tape = PlaytestInputTape::default();
        tape.start_recording(0);
        tape.note_polls(first, 1);
        tape.stop_recording(&path).unwrap();
        tape.start_recording(0);
        tape.note_polls(second, 1);
        tape.stop_recording(&path).unwrap();

        assert_eq!(read_input_tape(&path).unwrap().samples, [second]);
        let archived = std::fs::read_dir(root.join("archive"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(archived.len(), 1);
        assert_eq!(read_input_tape(&archived[0]).unwrap().samples, [first]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_stop_returns_replayable_csv_without_a_filesystem() {
        let frame = Port1PadSample {
            buttons: 0xffef,
            right_x: 0x70,
            right_y: 0x90,
            left_x: 0x60,
            left_y: 0xa0,
        };
        let mut tape = PlaytestInputTape::default();
        tape.start_recording(0);
        tape.note_polls(frame, 1);

        let (frames, csv) = tape.stop_recording_csv(Some(0xabcd));

        assert_eq!(frames, 1);
        assert!(!tape.is_recording());
        let parsed = emulator_core::tape_from_csv(&csv).unwrap();
        assert_eq!(parsed.samples, [frame]);
        assert_eq!(parsed.clock, TapeClock::PadPoll);
        assert_eq!(parsed.start_poll, 0);
        assert_eq!(parsed.game_hash, Some(0xabcd));
    }

    #[test]
    fn cold_boot_replay_feeds_idle_until_the_tape_start_poll() {
        let recorded = Port1PadSample {
            buttons: 0x4000,
            ..Port1PadSample::from_buttons(0)
        };
        let tape = emulator_core::input_tape::Tape {
            samples: vec![recorded],
            clock: TapeClock::PadPoll,
            start_poll: 2,
            game_hash: None,
        };
        let mut replay = PlaytestInputTape::default();
        assert_eq!(replay.start_replay_from_tape(tape).unwrap(), 1);

        // Live input must not leak through the pre-start window.
        let live = Port1PadSample::from_buttons(0x1234);
        let idle = Port1PadSample::from_buttons(0);
        assert_eq!(replay.sample_for_frame(live).0, idle);
        replay.note_polls(live, 1);
        assert_eq!(replay.sample_for_frame(live).0, idle);
        replay.note_polls(live, 1);

        // Poll 2 reached: sample 0 is due.
        assert_eq!(replay.sample_for_frame(live).0, recorded);
        replay.note_polls(live, 1);
        let (_, event) = replay.sample_for_frame(live);
        assert_eq!(event, Some(PlaytestInputEvent::ReplayFinished { frames: 1 }));
    }
}
