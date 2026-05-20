//! Embedded-play input recording and replay.
//!
//! Tapes store the exact DualShock port-1 state applied to the
//! emulated bus for each stepped video frame. Recording at this
//! boundary makes playback independent from host redraw cadence and
//! gives the editor a repeatable path for performance comparisons.

use std::path::Path;

use emulator_core::{Bus, ButtonState};
use psxed_ui::{EditorPlaytestTapeMode, EditorPlaytestTapeStatus};

const TAPE_MAGIC: &[u8; 8] = b"PXITAPE1";
const TAPE_HEADER_BYTES: usize = 12;
const TAPE_SAMPLE_BYTES: usize = 6;

/// One emulated frame's port-1 pad state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Port1PadSample {
    buttons: u16,
    right_x: u8,
    right_y: u8,
    left_x: u8,
    left_y: u8,
}

impl Port1PadSample {
    /// Build a bus-ready sample from host button and stick state.
    pub(crate) fn from_host(buttons: u16, right_stick: (f32, f32), left_stick: (f32, f32)) -> Self {
        let map_axis = |v: f32| ((v.clamp(-1.0, 1.0) * 127.0) + 128.0) as u8;
        let (rx, ry) = right_stick;
        let (lx, ly) = left_stick;
        Self {
            buttons,
            right_x: map_axis(rx),
            right_y: map_axis(-ry),
            left_x: map_axis(lx),
            left_y: map_axis(-ly),
        }
    }

    /// Apply this sample to the emulator's first controller port.
    pub(crate) fn apply_to_bus(self, bus: &mut Bus) {
        bus.set_port1_buttons(ButtonState::from_bits(self.buttons));
        bus.set_port1_sticks(self.right_x, self.right_y, self.left_x, self.left_y);
    }
}

/// Read a persisted editor playtest input tape.
pub(crate) fn read_input_tape(path: &Path) -> Result<Vec<Port1PadSample>, String> {
    read_tape(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaytestInputMode {
    Idle,
    Recording,
    Replaying,
}

impl Default for PlaytestInputMode {
    fn default() -> Self {
        Self::Idle
    }
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
}

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

fn write_tape(path: &Path, samples: &[Port1PadSample]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid input tape path: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    let sample_count = u32::try_from(samples.len())
        .map_err(|_| format!("input tape too large: {} frames", samples.len()))?;
    let mut bytes = Vec::with_capacity(TAPE_HEADER_BYTES + samples.len() * TAPE_SAMPLE_BYTES);
    bytes.extend_from_slice(TAPE_MAGIC);
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.buttons.to_le_bytes());
        bytes.push(sample.right_x);
        bytes.push(sample.right_y);
        bytes.push(sample.left_x);
        bytes.push(sample.left_y);
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn read_tape(path: &Path) -> Result<Vec<Port1PadSample>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() < TAPE_HEADER_BYTES {
        return Err(format!("{} is not a PSoXide input tape", path.display()));
    }
    if &bytes[..TAPE_MAGIC.len()] != TAPE_MAGIC {
        return Err(format!(
            "{} has an unknown input tape header",
            path.display()
        ));
    }
    let count = u32::from_le_bytes(
        bytes[TAPE_MAGIC.len()..TAPE_HEADER_BYTES]
            .try_into()
            .expect("header count slice length"),
    ) as usize;
    let expected_len = TAPE_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(TAPE_SAMPLE_BYTES)
                .ok_or_else(|| format!("{} input tape frame count overflows", path.display()))?,
        )
        .ok_or_else(|| format!("{} input tape length overflows", path.display()))?;
    if bytes.len() != expected_len {
        return Err(format!(
            "{} input tape length mismatch: expected {expected_len} bytes, got {}",
            path.display(),
            bytes.len()
        ));
    }

    let mut samples = Vec::with_capacity(count);
    let mut offset = TAPE_HEADER_BYTES;
    for _ in 0..count {
        let buttons = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        samples.push(Port1PadSample {
            buttons,
            right_x: bytes[offset + 2],
            right_y: bytes[offset + 3],
            left_x: bytes[offset + 4],
            left_y: bytes[offset + 5],
        });
        offset += TAPE_SAMPLE_BYTES;
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_sample_maps_sticks_to_dualshock_bytes() {
        let sample = Port1PadSample::from_host(0x1234, (1.0, -1.0), (-1.0, 0.0));

        assert_eq!(sample.buttons, 0x1234);
        assert_eq!(sample.right_x, 255);
        assert_eq!(sample.right_y, 255);
        assert_eq!(sample.left_x, 1);
        assert_eq!(sample.left_y, 128);
    }

    #[test]
    fn input_tape_round_trips_binary_file() {
        let path = std::env::temp_dir().join(format!(
            "psoxide-input-tape-test-{}.pxtape",
            std::process::id()
        ));
        let samples = vec![
            Port1PadSample::from_host(0x0001, (0.0, 0.0), (0.0, 0.0)),
            Port1PadSample::from_host(0x4000, (0.25, -0.5), (-0.25, 0.75)),
        ];

        write_tape(&path, &samples).expect("write tape");
        let loaded = read_tape(&path).expect("read tape");
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded, samples);
    }
}
