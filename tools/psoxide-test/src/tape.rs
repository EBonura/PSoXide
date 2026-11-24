//! `psoxide-test tape`: turn a frontend press schedule into a committed poll-bound tape.
//!
//! The frontend records each completed port-1 poll's sample while it plays the schedule
//! (`--press` with `--input-tape-transcribe`), then the harness replays the tape it wrote (same
//! pad, `--input-tape-hold-last`, same step count) and requires the same final hardware dump: the
//! tape is only kept when it reproduces the press run exactly. A schedule that moves a stick forces
//! the pad into analog at boot, so it is transcribed and replayed on the analog pad only.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::util;

/// The frontend's poll-bound tape format, `PXITAPE2`: magic, sample count (u32), the completed
/// port-1 poll at which sample 0 is fed (u32), then 6 bytes a sample (buttons u16, right x/y,
/// left x/y). Replay feeds nothing before `start_poll`, so the game reads an idle pad there.
pub const POLL_MAGIC: &[u8; 8] = b"PXITAPE2";
const POLL_HEADER: usize = 16;
const SAMPLE: usize = 6;

/// A poll-bound tape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollTape {
    pub start_poll: u32,
    pub samples: Vec<[u8; SAMPLE]>,
}

/// An idle pad: no button pressed, both sticks centred.
pub const IDLE: [u8; SAMPLE] = [0, 0, 0x80, 0x80, 0x80, 0x80];

impl PollTape {
    /// Read `PXITAPE2` bytes. A video-frame tape (`PXITAPE1`) or a CSV is refused: only a
    /// poll-bound tape keeps its route when the frame rate moves.
    pub fn parse(bytes: &[u8]) -> Result<PollTape, String> {
        if bytes.len() < POLL_HEADER || &bytes[..8] != POLL_MAGIC {
            return Err(
                "not a poll-bound tape (PXITAPE2): transcribe it with `psoxide-test \
                        tape`"
                    .into(),
            );
        }
        let word =
            |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a 4-byte slice"));
        let count = word(8) as usize;
        let start_poll = word(12);
        let body = &bytes[POLL_HEADER..];
        if body.len() != count * SAMPLE {
            return Err(format!(
                "tape length mismatch: {count} samples need {} bytes after the header, got {}",
                count * SAMPLE,
                body.len()
            ));
        }
        let samples = body
            .chunks_exact(SAMPLE)
            .map(|c| c.try_into().expect("a 4-byte chunk"))
            .collect();
        Ok(PollTape {
            start_poll,
            samples,
        })
    }

    /// The `PXITAPE2` bytes.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(POLL_HEADER + self.samples.len() * SAMPLE);
        out.extend_from_slice(POLL_MAGIC);
        out.extend_from_slice(&(self.samples.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.start_poll.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(s);
        }
        out
    }

    /// This tape fed from `anchor` polls later: what an anchored tape (whose polls count from
    /// the poll its anchor held at) is on a disc where the anchor held at poll `anchor`.
    pub fn after(&self, anchor: u32) -> Result<PollTape, String> {
        Ok(PollTape {
            start_poll: self
                .start_poll
                .checked_add(anchor)
                .ok_or("tape start poll overflows")?,
            samples: self.samples.clone(),
        })
    }

    /// Re-anchor a tape recorded from power-on at the poll its anchor held at: polls count from
    /// `anchor` instead of from power-on. Every sample before it must be an idle pad (the
    /// anchored replay feeds nothing there), so a tape that presses before its anchor is refused
    /// with the first such poll: pick an anchor that holds before the first press.
    pub fn anchored_at(&self, anchor: u32) -> Result<PollTape, String> {
        if self.start_poll >= anchor {
            return Ok(PollTape {
                start_poll: self.start_poll - anchor,
                samples: self.samples.clone(),
            });
        }
        let drop = (anchor - self.start_poll) as usize;
        if let Some((i, s)) = self
            .samples
            .iter()
            .take(drop)
            .enumerate()
            .find(|(_, s)| **s != IDLE)
        {
            return Err(format!(
                "the tape is not idle before poll {anchor}: poll {} feeds {:02x?} (an anchored \
                 replay feeds an idle pad until the anchor holds); anchor on a state the game \
                 reaches before its first press, or keep the presses with --before-out (an \
                 anchor after presses)",
                self.start_poll as usize + i,
                s
            ));
        }
        Ok(PollTape {
            start_poll: 0,
            samples: self.samples.iter().skip(drop).copied().collect(),
        })
    }
}

impl PollTape {
    /// The poll of the last sample that is not an idle pad, if any.
    #[must_use]
    pub fn last_press(&self) -> Option<u32> {
        self.samples
            .iter()
            .rposition(|s| *s != IDLE)
            .map(|i| self.start_poll + i as u32)
    }

    /// Split a tape recorded from power-on at the poll `anchor` held at in that recording: the
    /// part before it (fed from power-on while the anchor is looked for: the presses that reach
    /// the anchored state, ending idle) and the part from it (polls counted from the anchor). The
    /// anchor must come after the last press of the first part: a press on the anchor's poll or
    /// later belongs to the second.
    ///
    /// # Errors
    /// Nothing is pressed before `anchor` (use [`Self::anchored_at`]), or the tape ends before it.
    pub fn split_at_anchor(&self, anchor: u32) -> Result<(PollTape, PollTape), String> {
        let end = self.start_poll as usize + self.samples.len();
        if anchor as usize >= end || anchor < self.start_poll {
            return Err(format!(
                "poll {anchor} is outside the tape (polls {} to {})",
                self.start_poll,
                end.saturating_sub(1)
            ));
        }
        let cut = (anchor - self.start_poll) as usize;
        let mut before = PollTape {
            start_poll: self.start_poll,
            samples: self.samples[..cut].to_vec(),
        };
        if before.last_press().is_none() {
            return Err(format!(
                "nothing is pressed before poll {anchor}: anchor the tape with --at-poll alone"
            ));
        }
        // It ends idle: the replay looking for the anchor holds its last sample.
        before.samples.push(IDLE);
        let after = PollTape {
            start_poll: 0,
            samples: self.samples[cut..].to_vec(),
        };
        Ok((before, after))
    }

    /// The tape a window anchored after presses replays: `before` (fed from power-on, the
    /// presses that reach the anchored state) up to `anchor`, the poll its anchor first held at on
    /// this disc, idle after it, then `after` (polls counted from the anchor) from there.
    ///
    /// # Errors
    /// `before` presses on or after `anchor` (the anchor held before the presses meant to cause
    /// it), or the polls overflow.
    pub fn joined(before: &PollTape, after: &PollTape, anchor: u32) -> Result<PollTape, String> {
        if let Some(last) = before.last_press() {
            if last >= anchor {
                return Err(format!(
                    "the anchor held at poll {anchor}, but the anchor tape still presses at poll \
                     {last}: it held before the presses meant to cause it"
                ));
            }
        }
        if before.start_poll > anchor {
            return Err(format!(
                "the anchor tape starts at poll {}, after the anchor at poll {anchor}",
                before.start_poll
            ));
        }
        let first = anchor
            .checked_add(after.start_poll)
            .ok_or("tape start poll overflows")?;
        let mut samples: Vec<[u8; SAMPLE]> = before
            .samples
            .iter()
            .take((anchor - before.start_poll) as usize)
            .copied()
            .collect();
        samples.resize((first - before.start_poll) as usize, IDLE);
        samples.extend_from_slice(&after.samples);
        Ok(PollTape {
            start_poll: before.start_poll,
            samples,
        })
    }
}

/// `psoxide-test tape-anchor --tape IN --at-poll P --out OUT`: re-anchor a power-on tape at the
/// poll a `[[speed]]` window's `anchor` holds at on the current disc (the speed stage reports it).
pub fn anchor_command(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut tape = None;
    let mut at = None;
    let mut out = None;
    let mut before_out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value\nusage: {ANCHOR_USAGE}"))?;
        match flag.as_str() {
            "--tape" => tape = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--before-out" => before_out = Some(PathBuf::from(value)),
            "--at-poll" => {
                at = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("--at-poll `{value}` is not a number"))?,
                )
            }
            other => return Err(format!("unknown option `{other}`\nusage: {ANCHOR_USAGE}")),
        }
    }
    let missing = |what: &str| format!("{what} is required\nusage: {ANCHOR_USAGE}");
    let (tape, at, out) = (
        tape.ok_or_else(|| missing("--tape"))?,
        at.ok_or_else(|| missing("--at-poll"))?,
        out.ok_or_else(|| missing("--out"))?,
    );
    let bytes = std::fs::read(&tape).map_err(|e| format!("read {}: {e}", tape.display()))?;
    let recorded = PollTape::parse(&bytes).map_err(|e| format!("{}: {e}", tape.display()))?;
    if let Some(before_out) = before_out {
        // An anchor after presses: the presses that reach it go into their own tape.
        let (before, after) = recorded.split_at_anchor(at)?;
        std::fs::write(&before_out, before.bytes())
            .map_err(|e| format!("write {}: {e}", before_out.display()))?;
        std::fs::write(&out, after.bytes()).map_err(|e| format!("write {}: {e}", out.display()))?;
        println!(
            "wrote {} (the presses before the anchor, from power-on, last at poll {}) and {} ({} \
             samples from the anchor): declare them as the window's anchor_tape and tape",
            before_out.display(),
            before.last_press().unwrap_or(0),
            out.display(),
            after.samples.len()
        );
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let anchored = recorded.anchored_at(at)?;
    std::fs::write(&out, anchored.bytes()).map_err(|e| format!("write {}: {e}", out.display()))?;
    println!(
        "wrote {} ({} samples from the anchor, sample 0 at anchor + {}): declare the window's \
         polls from the anchor too",
        out.display(),
        anchored.samples.len(),
        anchored.start_poll
    );
    Ok(std::process::ExitCode::SUCCESS)
}

pub const ANCHOR_USAGE: &str =
    "psoxide-test tape-anchor --tape IN --at-poll P --out OUT [--before-out BEFORE]";

pub const JOIN_USAGE: &str =
    "psoxide-test tape-join --before BEFORE --tape AFTER --at-poll A --out JOINED";

/// `psoxide-test tape-join`: the tape a window anchored after presses replays on a disc where its
/// anchor first held at port-1 poll `A` ([`PollTape::joined`]), as the speed stage builds it.
pub fn join_command(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut paths: [Option<PathBuf>; 3] = [None, None, None];
    let mut at = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value\nusage: {JOIN_USAGE}"))?;
        match flag.as_str() {
            "--before" => paths[0] = Some(PathBuf::from(value)),
            "--tape" => paths[1] = Some(PathBuf::from(value)),
            "--out" => paths[2] = Some(PathBuf::from(value)),
            "--at-poll" => {
                at = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("--at-poll `{value}` is not a number"))?,
                )
            }
            other => return Err(format!("unknown option `{other}`\nusage: {JOIN_USAGE}")),
        }
    }
    let [Some(before), Some(after), Some(out)] = paths else {
        return Err(format!(
            "--before, --tape and --out are required\nusage: {JOIN_USAGE}"
        ));
    };
    let at = at.ok_or_else(|| format!("--at-poll is required\nusage: {JOIN_USAGE}"))?;
    let read = |p: &Path| -> Result<PollTape, String> {
        let bytes = std::fs::read(p).map_err(|e| format!("read {}: {e}", p.display()))?;
        PollTape::parse(&bytes).map_err(|e| format!("{}: {e}", p.display()))
    };
    let joined = PollTape::joined(&read(&before)?, &read(&after)?, at)?;
    std::fs::write(&out, joined.bytes()).map_err(|e| format!("write {}: {e}", out.display()))?;
    println!(
        "wrote {} ({} samples from poll {})",
        out.display(),
        joined.samples.len(),
        joined.start_poll
    );
    Ok(std::process::ExitCode::SUCCESS)
}

/// What `psoxide-test tape` was asked for.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub disc: PathBuf,
    pub press: String,
    pub out: PathBuf,
    pub steps: u64,
    /// Stop both runs at this port-1 poll (the frontend's `--stop-at-poll`); for games whose display
    /// start moves. Without it the runs end at `steps` (a game whose display start never moves).
    pub stop_at_poll: Option<u64>,
    /// `analog` (the frontend's default pad), `dualshock` or `digital`.
    pub pad: String,
}

pub const USAGE: &str = "psoxide-test tape --disc CUE --press SCHEDULE --out TAPE --steps N \
                         [--stop-at-poll P] [--pad analog|dualshock|digital]";

/// Read the command line after `tape`.
pub fn parse(args: &[String]) -> Result<Request, String> {
    let mut disc = None;
    let mut press = None;
    let mut out = None;
    let mut steps = None;
    let mut stop_at_poll = None;
    let mut pad = "analog".to_string();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\nusage: {USAGE}"))
        };
        match flag.as_str() {
            "--disc" => disc = Some(PathBuf::from(value()?)),
            "--press" => press = Some(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--steps" => {
                let v = value()?;
                steps = Some(
                    v.parse()
                        .map_err(|_| format!("--steps `{v}` is not a number"))?,
                );
            }
            "--stop-at-poll" => {
                let v = value()?;
                stop_at_poll = Some(
                    v.parse()
                        .map_err(|_| format!("--stop-at-poll `{v}` is not a number"))?,
                );
            }
            "--pad" => pad = value()?,
            other => return Err(format!("unknown option `{other}`\nusage: {USAGE}")),
        }
    }
    let missing = |what: &str| format!("{what} is required\nusage: {USAGE}");
    let request = Request {
        disc: disc.ok_or_else(|| missing("--disc"))?,
        press: press.ok_or_else(|| missing("--press"))?,
        out: out.ok_or_else(|| missing("--out"))?,
        steps: steps.ok_or_else(|| missing("--steps"))?,
        stop_at_poll,
        pad,
    };
    if !["analog", "dualshock", "digital"].contains(&request.pad.as_str()) {
        return Err(format!(
            "--pad `{}` is not analog, dualshock or digital",
            request.pad
        ));
    }
    if moves_a_stick(&request.press) && request.pad != "analog" {
        return Err(
            "the schedule moves a stick, which forces the pad into analog at boot: transcribe \
             and replay it with --pad analog"
                .into(),
        );
    }
    Ok(request)
}

/// Whether a `--press` schedule drives an analog stick.
pub fn moves_a_stick(press: &str) -> bool {
    press.contains("lstick=") || press.contains("rstick=")
}

fn pad_flags(pad: &str) -> Vec<String> {
    match pad {
        "digital" => vec!["--digital-pad".into()],
        "dualshock" => vec!["--dualshock-pad".into()],
        _ => Vec::new(),
    }
}

/// The two frontend runs: the press run that writes the tape and the replay that must equal it.
pub fn argv(frontend: &Path, r: &Request, dumps: [&Path; 2]) -> [Vec<String>; 2] {
    let head = |dump: &Path| {
        let mut v = vec![
            frontend.display().to_string(),
            "launch".into(),
            "--path".into(),
            r.disc.display().to_string(),
            "--steps".into(),
            r.steps.to_string(),
            "--dump-hw".into(),
            dump.display().to_string(),
        ];
        if let Some(poll) = r.stop_at_poll {
            v.push("--stop-at-poll".into());
            v.push(poll.to_string());
        }
        v.extend(pad_flags(&r.pad));
        v
    };
    let mut record = head(dumps[0]);
    record.extend([
        "--press".into(),
        r.press.clone(),
        "--input-tape-transcribe".into(),
        r.out.display().to_string(),
    ]);
    let mut replay = head(dumps[1]);
    replay.extend([
        "--input-tape".into(),
        r.out.display().to_string(),
        "--input-tape-hold-last".into(),
    ]);
    [record, replay]
}

/// Record the tape and prove it replays to the same machine state.
pub fn run(args: &[String]) -> Result<std::process::ExitCode, String> {
    let r = parse(args)?;
    let config = Config::load()?;
    let frontend = config
        .tool(&config.tools.frontend, "[tools] frontend")?
        .to_path_buf();
    let dir = config.work.join("tape");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let dumps = [dir.join("press.hw"), dir.join("replay.hw")];
    let [record, replay] = argv(&frontend, &r, [&dumps[0], &dumps[1]]);
    for (argv, what) in [(record, "press"), (replay, "replay")] {
        let log = dir.join(format!("{what}.log"));
        let done = util::run_logged(&argv, Path::new("."), &[], &log, None)?;
        if done.code != 0 {
            return Err(format!(
                "the {what} run exited {}; log {}",
                done.code,
                log.display()
            ));
        }
    }
    let a = util::sha256_file(&dumps[0])?;
    let b = util::sha256_file(&dumps[1])?;
    if a != b {
        let _ = std::fs::remove_file(&r.out);
        return Err(format!(
            "the tape does not reproduce the press run (hardware dumps {} and {}): removed {}",
            util::short(&a),
            util::short(&b),
            r.out.display()
        ));
    }
    println!(
        "wrote {} ({}): its replay ends in the same machine state as the press run (dump {})",
        r.out.display(),
        util::short(&util::sha256_file(&r.out)?),
        util::short(&a)
    );
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn a_request_is_read_and_a_stick_needs_the_analog_pad() {
        let r = parse(&args(
            "--disc d.cue --press 0x4000@45+12 --out t.pxtape --steps 900",
        ))
        .unwrap();
        assert_eq!((r.pad.as_str(), r.steps), ("analog", 900));
        assert!(parse(&args(
            "--disc d.cue --press 5:lstick=0/128 --out t --steps 9 --pad dualshock"
        ))
        .unwrap_err()
        .contains("--pad analog"));
        assert!(parse(&args("--disc d.cue --out t --steps 9"))
            .unwrap_err()
            .contains("--press"));
    }

    fn tape(start: u32, samples: &[[u8; 6]]) -> PollTape {
        PollTape {
            start_poll: start,
            samples: samples.to_vec(),
        }
    }
    const CROSS: [u8; 6] = [0, 0x40, 0x80, 0x80, 0x80, 0x80];

    #[test]
    fn a_poll_tape_round_trips_and_other_formats_are_refused() {
        let t = tape(7, &[IDLE, CROSS]);
        let bytes = t.bytes();
        assert_eq!(&bytes[..8], b"PXITAPE2");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 7);
        assert_eq!(PollTape::parse(&bytes).unwrap(), t);
        let mut video = bytes.clone();
        video[7] = b'1';
        assert!(PollTape::parse(&video).unwrap_err().contains("PXITAPE2"));
        assert!(PollTape::parse(&bytes[..bytes.len() - 1])
            .unwrap_err()
            .contains("mismatch"));
    }

    #[test]
    fn re_anchoring_drops_the_idle_polls_before_the_anchor_and_refuses_a_press() {
        // Recorded from power-on: idle through the boot, the first press at poll 3.
        let power_on = tape(0, &[IDLE, IDLE, IDLE, CROSS, IDLE]);
        let anchored = power_on.anchored_at(2).unwrap();
        assert_eq!(anchored, tape(0, &[IDLE, CROSS, IDLE]));
        // Fed after an anchor found at poll 2 it is the power-on tape again from that poll on
        // (the frontend feeds an idle pad before start_poll).
        assert_eq!(anchored.after(2).unwrap(), tape(2, &[IDLE, CROSS, IDLE]));
        // A boot core in front that adds 151 polls moves the anchor, not the tape.
        assert_eq!(anchored.after(153).unwrap().start_poll, 153);
        assert_eq!(anchored.after(153).unwrap().samples, anchored.samples);
        // An anchor after the first press would drop it: refused, naming the poll.
        let err = power_on.anchored_at(4).unwrap_err();
        assert!(err.contains("poll 3"), "{err}");
        // A tape that starts later than the anchor keeps its samples.
        assert_eq!(
            tape(10, &[CROSS]).anchored_at(4).unwrap(),
            tape(6, &[CROSS])
        );
    }

    /// QD Up's shot: Start and menu presses reach the table, the anchor holds when the table loop
    /// starts, the aim and the shot follow. On a disc whose table loads two polls later the
    /// presses after the anchor move with it; the presses before it stay where they were.
    #[test]
    fn an_anchor_after_presses_moves_what_follows_it_and_keeps_what_reaches_it() {
        const START: [u8; SAMPLE] = [0x08, 0, 0x80, 0x80, 0x80, 0x80];
        // Recorded from power-on: Start at poll 1, the table loads, the anchor held at poll 4,
        // cross (the shot) at poll 5.
        let recorded = tape(0, &[IDLE, START, IDLE, IDLE, IDLE, CROSS, IDLE]);
        assert_eq!(recorded.last_press(), Some(5));
        let (before, after) = recorded.split_at_anchor(4).unwrap();
        assert_eq!(before, tape(0, &[IDLE, START, IDLE, IDLE, IDLE]));
        assert_eq!(before.last_press(), Some(1));
        assert_eq!(after, tape(0, &[IDLE, CROSS, IDLE]));
        // The same disc: the join is the recording.
        assert_eq!(PollTape::joined(&before, &after, 4).unwrap(), recorded);
        // A slower load: the anchor at poll 6, the shot two polls later, Start where it was.
        let slower = PollTape::joined(&before, &after, 6).unwrap();
        assert_eq!(
            slower,
            tape(0, &[IDLE, START, IDLE, IDLE, IDLE, IDLE, IDLE, CROSS, IDLE])
        );
        // A faster one: the shot one poll earlier.
        assert_eq!(
            PollTape::joined(&before, &after, 3).unwrap(),
            tape(0, &[IDLE, START, IDLE, IDLE, CROSS, IDLE])
        );
        // An anchor that held on or before the last press meant to cause it: refused.
        let err = PollTape::joined(&before, &after, 1).unwrap_err();
        assert!(
            err.contains("before the presses meant to cause it"),
            "{err}"
        );
        // Nothing pressed before the anchor: the plain re-anchor is the tool.
        let err = tape(0, &[IDLE, IDLE, CROSS])
            .split_at_anchor(2)
            .unwrap_err();
        assert!(err.contains("--at-poll alone"), "{err}");
        // Outside the tape.
        assert!(recorded.split_at_anchor(9).is_err());
    }

    #[test]
    fn the_replay_uses_the_same_pad_and_holds_the_last_sample() {
        let r = parse(&args(
            "--disc d.cue --press 0x4000@45+12 --out t.pxtape --steps 900 --pad digital",
        ))
        .unwrap();
        let [rec, rep] = argv(Path::new("fe"), &r, [Path::new("a"), Path::new("b")]);
        assert!(rec.contains(&"--input-tape-transcribe".to_string()));
        assert!(!rec.contains(&"--input-tape".to_string()));
        for v in [&rec, &rep] {
            assert!(v.contains(&"--digital-pad".to_string()));
        }
        assert!(rep.contains(&"--input-tape-hold-last".to_string()));
    }
}
