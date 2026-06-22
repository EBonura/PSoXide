//! Input recording + replay for the editor playtest, built on the shared
//! [`emulator_core::input_tape`] `PXITAPE1` format. The same tapes drive
//! headless commercial-game / homebrew regression runs (`cli.rs
//! --input-tape`) and CI -- recording happens at the bus port-1 boundary,
//! which is game-agnostic.

use std::path::Path;

use emulator_core::input_tape::read_tape;
// `write_tape` only persists tapes recorded from the editor Play viewport.
#[cfg(feature = "editor")]
use emulator_core::input_tape::write_tape;
#[cfg(feature = "editor")]
use psxed_ui::{EditorPlaytestTapeMode, EditorPlaytestTapeStatus};

/// One emulated frame's port-1 pad state. Alias to the shared tape sample
/// so existing frontend call sites stay stable while the type + binary
/// format live in the core crate.
pub(crate) use emulator_core::input_tape::PadSample as Port1PadSample;

/// Read a persisted input tape. Only the headless CLI (`--input-tape`) reads
/// tapes from a path; that CLI is compiled out on wasm, so this is dead there.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn read_input_tape(path: &Path) -> Result<Vec<Port1PadSample>, String> {
    read_tape(path)
}

#[cfg(feature = "editor")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaytestInputMode {
    Idle,
    Recording,
    Replaying,
}

#[cfg(feature = "editor")]
impl Default for PlaytestInputMode {
    fn default() -> Self {
        Self::Idle
    }
}

/// One state transition emitted while applying a tape frame.
#[cfg(feature = "editor")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaytestInputEvent {
    /// Replay consumed the final recorded frame.
    ReplayFinished { frames: usize },
}

/// Mutable input tape state owned by the frontend app.
#[cfg(feature = "editor")]
#[derive(Debug, Default)]
pub(crate) struct PlaytestInputTape {
    mode: PlaytestInputMode,
    samples: Vec<Port1PadSample>,
    replay_cursor: usize,
}

#[cfg(feature = "editor")]
impl PlaytestInputTape {
    /// Editor-facing summary for controls and overlays.
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

    /// Start a new tape, discarding the in-memory previous recording.
    pub(crate) fn start_recording(&mut self) {
        self.samples.clear();
        self.replay_cursor = 0;
        self.mode = PlaytestInputMode::Recording;
    }

    /// Stop recording and persist the tape.
    pub(crate) fn stop_recording(&mut self, path: &Path) -> Result<usize, String> {
        let frames = self.samples.len();
        if self.mode == PlaytestInputMode::Recording {
            self.mode = PlaytestInputMode::Idle;
        }
        write_tape(path, &self.samples)?;
        Ok(frames)
    }

    /// Start replaying a persisted tape, falling back to memory.
    pub(crate) fn start_replay(&mut self, path: &Path) -> Result<usize, String> {
        if path.is_file() {
            self.samples = read_tape(path)?;
        }
        if self.samples.is_empty() {
            return Err("no recorded input tape found".to_string());
        }
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

    /// Stop any active tape mode, optionally saving an in-progress recording.
    pub(crate) fn stop_active(&mut self, path: &Path) -> Result<Option<usize>, String> {
        if self.is_recording() {
            self.stop_recording(path).map(Some)
        } else {
            self.stop_replay();
            Ok(None)
        }
    }

    /// Return the sample to apply for this emulated frame.
    pub(crate) fn sample_for_frame(
        &mut self,
        live_sample: Port1PadSample,
    ) -> (Port1PadSample, Option<PlaytestInputEvent>) {
        match self.mode {
            PlaytestInputMode::Idle => (live_sample, None),
            PlaytestInputMode::Recording => {
                self.samples.push(live_sample);
                (live_sample, None)
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
}
