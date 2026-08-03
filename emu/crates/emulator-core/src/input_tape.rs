//! Shared controller-input tape.
//!
//! A tape is the exact port-1 DualShock state applied to the emulated bus
//! for each stepped video frame. Recording at this boundary makes playback
//! independent of host redraw cadence and is fully **game-agnostic**: the
//! same format drives editor playtests of PSoXide-built homebrew,
//! commercial-game regression runs, and headless CI -- all inject through
//! `Bus::set_port1_*`, which neither knows nor cares what disc is mounted.
//!
//! Binary layout (`PXITAPE1`):
//! - offset 0:  8-byte magic `PXITAPE1`
//! - offset 8:  `u32` little-endian sample count
//! - offset 12: `count` samples, 6 bytes each
//!   (`u16` buttons LE, `right_x`, `right_y`, `left_x`, `left_y`)

use std::path::Path;

use crate::{Bus, ButtonState};

const TAPE_MAGIC: &[u8; 8] = b"PXITAPE1";
const TAPE_HEADER_BYTES: usize = 12;
/// Poll-bound tape: one sample per completed port-1 `0x42` poll, plus the
/// poll index the recording started at.
const TAPE_POLL_MAGIC: &[u8; 8] = b"PXITAPE2";
const TAPE_POLL_HEADER_BYTES: usize = 16;
const TAPE_SAMPLE_BYTES: usize = 6;
const TAPE_CSV_HEADER: &str = "frame,buttons,right_x,right_y,left_x,left_y";

/// Which clock a tape's samples are indexed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapeClock {
    /// `PXITAPE1`: one sample per emulated video frame. Faithful to when the
    /// host held each input, but the *guest* only sees the samples that happen
    /// to be live when it polls, so a build that renders at a different speed
    /// reads a shifted input stream and plays a different route.
    VideoFrame,
    /// `PXITAPE2`: one sample per completed port-1 pad poll. The guest reads
    /// the pad once per simulation tick, so sample N is delivered to poll N
    /// whatever the frame rate. Use this for performance A/B work, where the
    /// route must be identical between builds.
    PadPoll,
}

/// A tape plus the clock its samples are indexed by.
#[derive(Clone, Debug)]
pub struct Tape {
    /// Pad state, one entry per tick of `clock`.
    pub samples: Vec<PadSample>,
    /// Which clock indexes `samples`.
    pub clock: TapeClock,
    /// For [`TapeClock::PadPoll`], the port-1 poll index that sample 0 belongs
    /// to; replay feeds nothing until the guest reaches it. Zero otherwise.
    pub start_poll: u64,
}

/// One emulated frame's port-1 DualShock state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadSample {
    /// Digital button bitmask (see [`crate::pad::button`]).
    pub buttons: u16,
    /// Right analog stick X (0x00 left .. 0x80 center .. 0xFF right).
    pub right_x: u8,
    /// Right analog stick Y (0x00 up .. 0x80 center .. 0xFF down).
    pub right_y: u8,
    /// Left analog stick X (0x00 left .. 0x80 center .. 0xFF right).
    pub left_x: u8,
    /// Left analog stick Y (0x00 up .. 0x80 center .. 0xFF down).
    pub left_y: u8,
}

impl PadSample {
    /// Digital-only sample with both analog sticks centered. Handy for
    /// hand-authored test tapes that only press buttons.
    pub fn from_buttons(buttons: u16) -> Self {
        Self {
            buttons,
            right_x: 0x80,
            right_y: 0x80,
            left_x: 0x80,
            left_y: 0x80,
        }
    }

    /// Build a bus-ready sample from host button + stick state. Sticks are
    /// `-1.0..=1.0`; Y is inverted to the DualShock byte convention.
    pub fn from_host(buttons: u16, right_stick: (f32, f32), left_stick: (f32, f32)) -> Self {
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
    pub fn apply_to_bus(self, bus: &mut Bus) {
        bus.set_port1_buttons(ButtonState::from_bits(self.buttons));
        bus.set_port1_sticks(self.right_x, self.right_y, self.left_x, self.left_y);
    }
}

/// Serialize samples to a `PXITAPE1` file.
pub fn write_tape(path: &Path, samples: &[PadSample]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let sample_count = u32::try_from(samples.len())
        .map_err(|_| format!("input tape too large: {} frames", samples.len()))?;
    let mut bytes = Vec::with_capacity(TAPE_HEADER_BYTES + samples.len() * TAPE_SAMPLE_BYTES);
    bytes.extend_from_slice(TAPE_MAGIC);
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.buttons.to_le_bytes());
        bytes.push(s.right_x);
        bytes.push(s.right_y);
        bytes.push(s.left_x);
        bytes.push(s.left_y);
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Serialize samples to the human-readable CSV format used by browser
/// downloads. `buttons` is the raw active-low PlayStation pad mask.
///
/// The CSV carries no clock marker, so a round-tripped tape reads back as
/// [`TapeClock::VideoFrame`]. Use the binary `PXITAPE2` form for anything that
/// must replay identically across builds of differing speed.
pub fn tape_to_csv(samples: &[PadSample]) -> String {
    use std::fmt::Write as _;

    let mut csv = String::with_capacity(TAPE_CSV_HEADER.len() + 1 + samples.len() * 28);
    csv.push_str(TAPE_CSV_HEADER);
    csv.push('\n');
    for (frame, sample) in samples.iter().enumerate() {
        let _ = writeln!(
            csv,
            "{frame},{},{},{},{},{}",
            sample.buttons, sample.right_x, sample.right_y, sample.left_x, sample.left_y
        );
    }
    csv
}

/// Parse a browser-exported input CSV back into frame samples.
pub fn tape_from_csv(csv: &str) -> Result<Vec<PadSample>, String> {
    let mut lines = csv.lines();
    let header = lines
        .next()
        .map(str::trim)
        .ok_or_else(|| "input tape CSV is empty".to_string())?;
    if header != TAPE_CSV_HEADER {
        return Err(format!(
            "unknown input tape CSV header: expected {TAPE_CSV_HEADER}"
        ));
    }

    let mut samples = Vec::new();
    for (line_index, line) in lines.enumerate() {
        let line_number = line_index + 2;
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 6 {
            return Err(format!(
                "input tape CSV line {line_number} has {} fields, expected 6",
                fields.len()
            ));
        }
        let frame = fields[0]
            .parse::<usize>()
            .map_err(|error| format!("input tape CSV line {line_number} frame: {error}"))?;
        if frame != samples.len() {
            return Err(format!(
                "input tape CSV line {line_number} has frame {frame}, expected {}",
                samples.len()
            ));
        }
        let parse_u16 = |field: &str, name: &str| {
            field
                .parse::<u16>()
                .map_err(|error| format!("input tape CSV line {line_number} {name}: {error}"))
        };
        let parse_u8 = |field: &str, name: &str| {
            field
                .parse::<u8>()
                .map_err(|error| format!("input tape CSV line {line_number} {name}: {error}"))
        };
        samples.push(PadSample {
            buttons: parse_u16(fields[1], "buttons")?,
            right_x: parse_u8(fields[2], "right_x")?,
            right_y: parse_u8(fields[3], "right_y")?,
            left_x: parse_u8(fields[4], "left_x")?,
            left_y: parse_u8(fields[5], "left_y")?,
        });
    }
    Ok(samples)
}

/// Serialize poll-bound samples as `PXITAPE2`. `start_poll` is the port-1
/// completed-poll count at which sample 0 was captured.
pub fn write_tape_poll_bound(
    path: &Path,
    samples: &[PadSample],
    start_poll: u64,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let sample_count = u32::try_from(samples.len())
        .map_err(|_| format!("input tape too large: {} polls", samples.len()))?;
    let start = u32::try_from(start_poll)
        .map_err(|_| format!("input tape start poll too large: {start_poll}"))?;
    let mut bytes = Vec::with_capacity(TAPE_POLL_HEADER_BYTES + samples.len() * TAPE_SAMPLE_BYTES);
    bytes.extend_from_slice(TAPE_POLL_MAGIC);
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    bytes.extend_from_slice(&start.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.buttons.to_le_bytes());
        bytes.push(s.right_x);
        bytes.push(s.right_y);
        bytes.push(s.left_x);
        bytes.push(s.left_y);
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Read a tape of either clock. CSV and `PXITAPE1` files report
/// [`TapeClock::VideoFrame`].
pub fn read_tape_full(path: &Path) -> Result<Tape, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() >= TAPE_POLL_HEADER_BYTES && &bytes[..TAPE_POLL_MAGIC.len()] == TAPE_POLL_MAGIC {
        let count = u32::from_le_bytes(bytes[8..12].try_into().expect("count slice")) as usize;
        let start_poll = u32::from_le_bytes(bytes[12..16].try_into().expect("start slice")) as u64;
        let expected_len = TAPE_POLL_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(TAPE_SAMPLE_BYTES)
                    .ok_or_else(|| format!("{} poll count overflows", path.display()))?,
            )
            .ok_or_else(|| format!("{} length overflows", path.display()))?;
        if bytes.len() != expected_len {
            return Err(format!(
                "{} length mismatch: expected {expected_len} bytes, got {}",
                path.display(),
                bytes.len()
            ));
        }
        let mut samples = Vec::with_capacity(count);
        let mut off = TAPE_POLL_HEADER_BYTES;
        for _ in 0..count {
            samples.push(PadSample {
                buttons: u16::from_le_bytes([bytes[off], bytes[off + 1]]),
                right_x: bytes[off + 2],
                right_y: bytes[off + 3],
                left_x: bytes[off + 4],
                left_y: bytes[off + 5],
            });
            off += TAPE_SAMPLE_BYTES;
        }
        return Ok(Tape {
            samples,
            clock: TapeClock::PadPoll,
            start_poll,
        });
    }
    Ok(Tape {
        samples: read_tape(path)?,
        clock: TapeClock::VideoFrame,
        start_poll: 0,
    })
}

/// Read a `PXITAPE1` file into samples.
pub fn read_tape(path: &Path) -> Result<Vec<PadSample>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        let csv = std::str::from_utf8(&bytes)
            .map_err(|error| format!("read input tape CSV {}: {error}", path.display()))?;
        return tape_from_csv(csv)
            .map_err(|error| format!("read input tape CSV {}: {error}", path.display()));
    }
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
                .ok_or_else(|| format!("{} frame count overflows", path.display()))?,
        )
        .ok_or_else(|| format!("{} length overflows", path.display()))?;
    if bytes.len() != expected_len {
        return Err(format!(
            "{} length mismatch: expected {expected_len} bytes, got {}",
            path.display(),
            bytes.len()
        ));
    }
    let mut samples = Vec::with_capacity(count);
    let mut off = TAPE_HEADER_BYTES;
    for _ in 0..count {
        samples.push(PadSample {
            buttons: u16::from_le_bytes([bytes[off], bytes[off + 1]]),
            right_x: bytes[off + 2],
            right_y: bytes[off + 3],
            left_x: bytes[off + 4],
            left_y: bytes[off + 5],
        });
        off += TAPE_SAMPLE_BYTES;
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_sample_maps_sticks_to_dualshock_bytes() {
        let s = PadSample::from_host(0x1234, (1.0, -1.0), (-1.0, 0.0));
        assert_eq!(s.buttons, 0x1234);
        assert_eq!(s.right_x, 255);
        assert_eq!(s.right_y, 255);
        assert_eq!(s.left_x, 1);
        assert_eq!(s.left_y, 128);
    }

    #[test]
    fn from_buttons_centers_sticks() {
        let s = PadSample::from_buttons(0x00FF);
        assert_eq!(
            (s.right_x, s.right_y, s.left_x, s.left_y),
            (0x80, 0x80, 0x80, 0x80)
        );
    }

    #[test]
    fn tape_round_trips_binary_file() {
        let path = std::env::temp_dir().join(format!("psoxide-tape-{}.pxtape", std::process::id()));
        let samples = vec![
            PadSample::from_host(0x0001, (0.0, 0.0), (0.0, 0.0)),
            PadSample::from_host(0x4000, (0.25, -0.5), (-0.25, 0.75)),
        ];
        write_tape(&path, &samples).expect("write");
        let loaded = read_tape(&path).expect("read");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, samples);
    }

    #[test]
    fn tape_round_trips_browser_csv() {
        let samples = vec![
            PadSample::from_host(0xffef, (0.0, 0.0), (-1.0, 1.0)),
            PadSample::from_host(0x7fff, (1.0, -1.0), (0.25, -0.25)),
        ];
        let csv = tape_to_csv(&samples);
        assert_eq!(
            csv.lines().next(),
            Some("frame,buttons,right_x,right_y,left_x,left_y")
        );
        assert_eq!(tape_from_csv(&csv).unwrap(), samples);
    }

    #[test]
    fn read_tape_accepts_browser_csv_files() {
        let path = std::env::temp_dir().join(format!("psoxide-tape-{}.csv", std::process::id()));
        let samples = [PadSample::from_buttons(0xfffe)];
        std::fs::write(&path, tape_to_csv(&samples)).expect("write CSV tape");
        let loaded = read_tape(&path).expect("read CSV tape");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, samples);
    }

    #[test]
    fn browser_csv_rejects_discontinuous_frames() {
        let csv = concat!(
            "frame,buttons,right_x,right_y,left_x,left_y\n",
            "1,65535,128,128,128,128\n"
        );
        assert!(tape_from_csv(csv)
            .unwrap_err()
            .contains("has frame 1, expected 0"));
    }
}
