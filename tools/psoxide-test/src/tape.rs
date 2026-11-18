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

/// What `psoxide-test tape` was asked for.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub disc: PathBuf,
    pub press: String,
    pub out: PathBuf,
    pub steps: u64,
    /// `analog` (the frontend's default pad), `dualshock` or `digital`.
    pub pad: String,
}

pub const USAGE: &str = "psoxide-test tape --disc CUE --press SCHEDULE --out TAPE --steps N \
                         [--pad analog|dualshock|digital]";

/// Read the command line after `tape`.
pub fn parse(args: &[String]) -> Result<Request, String> {
    let mut disc = None;
    let mut press = None;
    let mut out = None;
    let mut steps = None;
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
