//! `psoxide-perf adopt-pgo`: keep a newly collected PGO ticket only when it benches no worse
//! than the one it would replace.
//!
//! A ticket is the set of files that steer a game's profile-guided link: the sample profile, the
//! hot call-site threshold, the I-cache layout. Two profiles that differ by under 2% on any
//! function can put a few call sites on opposite sides of the hot threshold and flip inlining in
//! a large function; Counter-Strike measured a fresh collection 4.4% slower on one map than the
//! profile it replaced, at the same SDK code and rung. Nothing in the collection flows checked,
//! so every collection was a lottery ticket.
//!
//! The game collects as it always has, into its working tree. This command then reads the
//! incumbent from git (`--rev`, default `HEAD`), builds and benches both with the game's own
//! build command and `psoxide-pgo measure`, and leaves in the tree whichever wins: the
//! candidate only when it is no slower than the tie band on any window and faster than the
//! band overall. A tie keeps the incumbent. The comparison (every score, the decision and why)
//! is written to `--record`, which the game commits beside the ticket.
//!
//! The score is `work_per_frame` (work cycles per presented frame, waits removed), the mean over
//! several poll offsets of each window: one window edge can land on either side of a vblank
//! bucket, and averaging a few shifted windows removes that, where a single p99 does not.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::util::{Error, Result};

/// Poll offsets each window is measured at, unless `--offsets` says otherwise.
pub(crate) const DEFAULT_OFFSETS: [i64; 4] = [-48, -24, 0, 24];
/// Differences inside this percentage are a tie, unless `--tie` says otherwise. Two links of the
/// same code measured within 0.14% of each other this way on Counter-Strike.
pub(crate) const DEFAULT_TIE_PERCENT: f64 = 0.25;

const HELP: &str = "usage: psoxide-perf adopt-pgo --repo DIR [--rev REV] (--file PATH)...\n\
    --build CMD --image PATH --frontend PATH (--tape PATH --polls A..B)...\n\
    [--launch-arg ARG]... [--offsets O,O,...] [--tie PERCENT] [--jobs N]\n\
    [--timeout SECONDS] [--pgo PATH] [--work DIR] --record PATH\n\
  Benches the ticket committed at REV against the one in the working tree and keeps the\n\
  candidate only when it is no slower on any window and faster overall (see README).";

/// One gameplay window of a bench tape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    /// The tape's file stem; names the window in the record.
    pub name: String,
    /// The poll-bound input tape.
    pub tape: PathBuf,
    /// First port-1 poll of the window.
    pub from: u64,
    /// Last port-1 poll of the window.
    pub to: u64,
}

/// Which ticket a build or bench is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// The ticket just collected, in the working tree.
    Candidate,
    /// The ticket committed at the revision.
    Incumbent,
}

impl Side {
    /// The side's name in the record and in `PSOXIDE_ADOPT_SIDE`.
    pub fn label(self) -> &'static str {
        match self {
            Side::Candidate => "candidate",
            Side::Incumbent => "incumbent",
        }
    }
}

/// One file of the ticket, both versions. `None` means the file does not exist on that side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TicketFile {
    /// Path relative to the repository.
    pub path: String,
    /// Contents at the revision.
    pub incumbent: Option<Vec<u8>>,
    /// Contents in the working tree.
    pub candidate: Option<Vec<u8>>,
}

impl TicketFile {
    fn contents(&self, side: Side) -> Option<&[u8]> {
        match side {
            Side::Candidate => self.candidate.as_deref(),
            Side::Incumbent => self.incumbent.as_deref(),
        }
    }
}

/// What the comparison is run with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    /// The bench windows.
    pub windows: Vec<Window>,
    /// Poll offsets each window is measured at.
    pub offsets: Vec<i64>,
    /// Differences inside this percentage tie.
    pub tie_percent: f64,
}

/// The build and the bench, so the decision tests without a compiler or an emulator.
pub(crate) trait Rig {
    /// Build the ticket now in the tree. `Err` carries why it failed.
    fn build(&mut self, side: Side) -> std::result::Result<(), String>;
    /// Measure `work_per_frame` for each (window, offset), in order.
    fn bench(
        &mut self,
        side: Side,
        runs: &[(&Window, i64)],
    ) -> Vec<std::result::Result<f64, String>>;
}

/// What happened to one side.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bench {
    /// `Err` when the build failed, with why.
    pub built: std::result::Result<(), String>,
    /// `work_per_frame` per window, per offset, when the bench ran in full.
    pub scores: Vec<Vec<f64>>,
    /// Why the bench did not run in full.
    pub failure: Option<String>,
}

impl Bench {
    fn ok(&self) -> bool {
        self.built.is_ok() && self.failure.is_none()
    }

    fn means(&self) -> Vec<f64> {
        self.scores
            .iter()
            .map(|runs| runs.iter().sum::<f64>() / runs.len() as f64)
            .collect()
    }

    fn why_not(&self) -> String {
        match (&self.built, &self.failure) {
            (Err(why), _) => format!("did not build: {why}"),
            (Ok(()), Some(why)) => format!("did not bench: {why}"),
            (Ok(()), None) => "benched".to_string(),
        }
    }
}

/// The verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The candidate stays in the tree.
    Adopt,
    /// The incumbent is put back.
    KeepIncumbent,
    /// The candidate is byte-identical to the incumbent: nothing to decide.
    Unchanged,
}

impl Decision {
    /// The decision's name in the record.
    pub fn label(self) -> &'static str {
        match self {
            Decision::Adopt => "adopt",
            Decision::KeepIncumbent => "keep-incumbent",
            Decision::Unchanged => "unchanged",
        }
    }
}

/// The comparison and its verdict.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Outcome {
    /// The verdict.
    pub decision: Decision,
    /// Why, in one sentence.
    pub reason: String,
    /// The candidate's build and bench, when it was built.
    pub candidate: Option<Bench>,
    /// The incumbent's build and bench, when it was built.
    pub incumbent: Option<Bench>,
    /// Candidate mean over incumbent mean, per window, when both benched.
    pub ratios: Vec<f64>,
    /// Geometric mean of `ratios`.
    pub geomean: Option<f64>,
}

/// Put one side's files in the tree under `root` (absent files are removed).
pub(crate) fn put(root: &Path, files: &[TicketFile], side: Side) -> Result<()> {
    for file in files {
        let path = root.join(&file.path);
        match file.contents(side) {
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, bytes)?;
            }
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Error(format!("{}: {error}", path.display())));
                }
                _ => {}
            },
        }
    }
    Ok(())
}

fn run_side(
    root: &Path,
    files: &[TicketFile],
    side: Side,
    config: &Config,
    rig: &mut dyn Rig,
) -> Result<Bench> {
    put(root, files, side)?;
    if let Err(why) = rig.build(side) {
        return Ok(Bench {
            built: Err(why),
            scores: Vec::new(),
            failure: None,
        });
    }
    let runs: Vec<(&Window, i64)> = config
        .windows
        .iter()
        .flat_map(|window| config.offsets.iter().map(move |&offset| (window, offset)))
        .collect();
    let results = rig.bench(side, &runs);
    let mut scores = vec![Vec::new(); config.windows.len()];
    let mut failure = None;
    for (index, result) in results.into_iter().enumerate() {
        let (window, offset) = runs[index];
        match result {
            Ok(value) if value > 0.0 => {
                scores[index / config.offsets.len()].push(value);
            }
            Ok(value) => {
                failure.get_or_insert(format!("{} at offset {offset}: score {value}", window.name));
            }
            Err(why) => {
                failure.get_or_insert(format!("{} at offset {offset}: {why}", window.name));
            }
        }
    }
    if results_len_mismatch(&scores, config) && failure.is_none() {
        failure = Some("the bench returned fewer results than runs".to_string());
    }
    Ok(Bench {
        built: Ok(()),
        scores: if failure.is_none() {
            scores
        } else {
            Vec::new()
        },
        failure,
    })
}

fn results_len_mismatch(scores: &[Vec<f64>], config: &Config) -> bool {
    scores.iter().any(|runs| runs.len() != config.offsets.len())
}

/// Bench both tickets and leave the winner in the tree under `root`.
pub(crate) fn decide(
    root: &Path,
    files: &[TicketFile],
    config: &Config,
    rig: &mut dyn Rig,
) -> Result<Outcome> {
    if config.windows.is_empty() || config.offsets.is_empty() {
        return Err("adopt-pgo needs at least one --tape window and one offset".into());
    }
    if !(config.tie_percent >= 0.0 && config.tie_percent < 100.0) {
        return Err(format!("--tie must be in 0..100, not {}", config.tie_percent).into());
    }
    let outcome = |decision, reason: String, candidate, incumbent| Outcome {
        decision,
        reason,
        candidate,
        incumbent,
        ratios: Vec::new(),
        geomean: None,
    };
    if files.iter().all(|file| file.candidate == file.incumbent) {
        return Ok(outcome(
            Decision::Unchanged,
            "the candidate is byte-identical to the incumbent".into(),
            None,
            None,
        ));
    }
    if files.iter().all(|file| file.incumbent.is_none()) {
        put(root, files, Side::Candidate)?;
        return Ok(outcome(
            Decision::Adopt,
            "no incumbent is committed at the revision".into(),
            None,
            None,
        ));
    }
    // The candidate first: when it loses (the case this exists for), the incumbent is the last
    // build and nothing is rebuilt. Whatever the verdict, the last build is the winner, so the
    // image on disk matches the files left in the tree.
    let candidate = run_side(root, files, Side::Candidate, config, rig)?;
    if !candidate.ok() {
        rebuild(root, files, Side::Incumbent, rig)?;
        let reason = format!("the candidate {}", candidate.why_not());
        return Ok(outcome(
            Decision::KeepIncumbent,
            reason,
            Some(candidate),
            None,
        ));
    }
    let incumbent = run_side(root, files, Side::Incumbent, config, rig)?;
    if !incumbent.ok() {
        rebuild(root, files, Side::Candidate, rig)?;
        let reason = format!(
            "the incumbent {} on this tree, so it cannot be kept",
            incumbent.why_not()
        );
        return Ok(outcome(
            Decision::Adopt,
            reason,
            Some(candidate),
            Some(incumbent),
        ));
    }
    let ratios: Vec<f64> = candidate
        .means()
        .iter()
        .zip(incumbent.means())
        .map(|(new, old)| new / old)
        .collect();
    let geomean = (ratios.iter().map(|r| r.ln()).sum::<f64>() / ratios.len() as f64).exp();
    let band = config.tie_percent / 100.0;
    let worst = ratios
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(index, ratio)| (&config.windows[index].name, *ratio))
        .expect("at least one window");
    let (decision, reason) = if worst.1 > 1.0 + band {
        (
            Decision::KeepIncumbent,
            format!(
                "the candidate is slower on {} by {}, outside the {}% tie band",
                worst.0,
                percent(worst.1),
                config.tie_percent
            ),
        )
    } else if geomean >= 1.0 - band {
        (
            Decision::KeepIncumbent,
            format!(
                "the candidate ties (geomean {}, inside the {}% band): the incumbent stays",
                percent(geomean),
                config.tie_percent
            ),
        )
    } else {
        (
            Decision::Adopt,
            format!(
                "the candidate is faster overall (geomean {}) and no window is slower than the {}% band",
                percent(geomean),
                config.tie_percent
            ),
        )
    };
    if decision == Decision::Adopt {
        rebuild(root, files, Side::Candidate, rig)?;
    }
    Ok(Outcome {
        decision,
        reason,
        candidate: Some(candidate),
        incumbent: Some(incumbent),
        ratios,
        geomean: Some(geomean),
    })
}

/// Put `side` back in the tree and build it, so the image on disk is the winner's.
fn rebuild(root: &Path, files: &[TicketFile], side: Side, rig: &mut dyn Rig) -> Result<()> {
    put(root, files, side)?;
    rig.build(side).map_err(|why| {
        Error(format!(
            "the {} was kept but did not build again: {why}",
            side.label()
        ))
    })
}

fn percent(ratio: f64) -> String {
    format!("{:+.2}%", 100.0 * (ratio - 1.0))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where the comparison ran, for the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Provenance {
    /// The full commit the incumbent was read from.
    pub rev: String,
    /// SHA-256 of the frontend binary that ran the benches.
    pub frontend_sha256: String,
}

/// The record a game commits beside its ticket. It holds no time, path or host, so the same
/// comparison writes the same bytes.
pub(crate) fn record(
    outcome: &Outcome,
    files: &[TicketFile],
    config: &Config,
    provenance: &Provenance,
) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# psoxide-perf adopt-pgo: a collected PGO ticket against the committed one"
    );
    let _ = writeln!(text, "decision {}", outcome.decision.label());
    let _ = writeln!(text, "reason {}", outcome.reason);
    let _ = writeln!(text, "rev {}", provenance.rev);
    let _ = writeln!(
        text,
        "metric work_per_frame (psoxide-pgo measure, lower is better), mean over the offsets"
    );
    let _ = writeln!(text, "tie_percent {}", config.tie_percent);
    let offsets: Vec<String> = config.offsets.iter().map(i64::to_string).collect();
    let _ = writeln!(text, "offsets {}", offsets.join(" "));
    let _ = writeln!(text, "frontend_sha256 {}", provenance.frontend_sha256);
    for file in files {
        let hash = |contents: &Option<Vec<u8>>| {
            contents.as_deref().map_or("absent".to_string(), sha256_hex)
        };
        let _ = writeln!(
            text,
            "file {} incumbent {} candidate {}",
            file.path,
            hash(&file.incumbent),
            hash(&file.candidate)
        );
    }
    for window in &config.windows {
        let _ = writeln!(
            text,
            "window {} polls {}..{}",
            window.name, window.from, window.to
        );
    }
    for (side, bench) in [
        (Side::Candidate, &outcome.candidate),
        (Side::Incumbent, &outcome.incumbent),
    ] {
        let Some(bench) = bench else {
            let _ = writeln!(text, "{} not built", side.label());
            continue;
        };
        let _ = writeln!(text, "{} {}", side.label(), bench.why_not());
        for (window, runs) in config.windows.iter().zip(&bench.scores) {
            for (offset, value) in config.offsets.iter().zip(runs) {
                let _ = writeln!(
                    text,
                    "score {} {} {offset} {value:.0}",
                    side.label(),
                    window.name
                );
            }
        }
        for (window, mean) in config.windows.iter().zip(bench.means()) {
            let _ = writeln!(text, "mean {} {} {mean:.1}", side.label(), window.name);
        }
    }
    for (window, ratio) in config.windows.iter().zip(&outcome.ratios) {
        let _ = writeln!(
            text,
            "candidate_vs_incumbent {} {}",
            window.name,
            percent(*ratio)
        );
    }
    if let Some(geomean) = outcome.geomean {
        let _ = writeln!(text, "candidate_vs_incumbent geomean {}", percent(geomean));
    }
    text
}

/// The real rig: the game's build command and `psoxide-pgo measure`.
pub(crate) struct CommandRig {
    /// Where the build command runs.
    pub root: PathBuf,
    /// Shell command that builds the ticket in the tree.
    pub build: String,
    /// The image the build leaves (a `.cue` or `.exe`).
    pub image: PathBuf,
    /// The emulator frontend.
    pub frontend: PathBuf,
    /// `psoxide-pgo`.
    pub pgo: PathBuf,
    /// Extra `--launch-arg`s for every replay.
    pub launch_args: Vec<String>,
    /// Concurrent replays.
    pub jobs: usize,
    /// Per-replay limit.
    pub timeout: Duration,
    /// Logs.
    pub work: PathBuf,
}

fn last_line(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(no output)")
        .trim()
        .to_string()
}

impl Rig for CommandRig {
    fn build(&mut self, side: Side) -> std::result::Result<(), String> {
        let log = self.work.join(format!("build-{}.log", side.label()));
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let err = file.try_clone().map_err(|e| e.to_string())?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(&self.build)
            .current_dir(&self.root)
            .env("PSOXIDE_ADOPT_SIDE", side.label())
            .stdout(file)
            .stderr(err)
            .status()
            .map_err(|e| format!("could not run the build: {e}"))?;
        if !status.success() {
            return Err(format!(
                "{status}: {} (log {})",
                last_line(&log),
                log.display()
            ));
        }
        if !self.image.is_file() {
            return Err(format!(
                "the build left no image at {}",
                self.image.display()
            ));
        }
        Ok(())
    }

    fn bench(
        &mut self,
        side: Side,
        runs: &[(&Window, i64)],
    ) -> Vec<std::result::Result<f64, String>> {
        let mut results: Vec<Option<std::result::Result<f64, String>>> = vec![None; runs.len()];
        let mut running: Vec<(usize, Child, Instant, PathBuf)> = Vec::new();
        let mut next = 0;
        while next < runs.len() || !running.is_empty() {
            while next < runs.len() && running.len() < self.jobs.max(1) {
                let (window, offset) = runs[next];
                match self.spawn(side, window, offset) {
                    Ok((child, out)) => running.push((next, child, Instant::now(), out)),
                    Err(why) => results[next] = Some(Err(why)),
                }
                next += 1;
            }
            let mut index = 0;
            while index < running.len() {
                let (run, child, started, out) = &mut running[index];
                let done = match child.try_wait() {
                    Ok(Some(status)) if status.success() => Some(score(out)),
                    Ok(Some(status)) => Some(Err(format!(
                        "measure {status}: {}",
                        last_line(&out.with_extension("err"))
                    ))),
                    Ok(None) if started.elapsed() > self.timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        Some(Err(format!("measure ran past {:?}", self.timeout)))
                    }
                    Ok(None) => None,
                    Err(error) => Some(Err(error.to_string())),
                };
                match done {
                    Some(result) => {
                        results[*run] = Some(result);
                        running.swap_remove(index);
                    }
                    None => index += 1,
                }
            }
            if !running.is_empty() {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        results
            .into_iter()
            .map(|result| result.unwrap_or_else(|| Err("not run".to_string())))
            .collect()
    }
}

impl CommandRig {
    fn spawn(
        &self,
        side: Side,
        window: &Window,
        offset: i64,
    ) -> std::result::Result<(Child, PathBuf), String> {
        let shift = |poll: u64| {
            u64::try_from(poll as i64 + offset)
                .map_err(|_| format!("{} shifted by {offset} starts before poll 0", window.name))
        };
        let (from, to) = (shift(window.from)?, shift(window.to)?);
        let stem = format!("{}-{}-{offset}", side.label(), window.name);
        let out = self.work.join(format!("{stem}.out"));
        let stdout = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        let stderr = std::fs::File::create(out.with_extension("err")).map_err(|e| e.to_string())?;
        let mut command = Command::new(&self.pgo);
        command
            .arg("measure")
            .arg("--frontend")
            .arg(&self.frontend)
            .arg("--image")
            .arg(&self.image)
            .arg("--tape")
            .arg(&window.tape)
            .arg("--polls")
            .arg(format!("{from}..{to}"))
            .args(["--name", "w"]);
        for arg in &self.launch_args {
            command.arg("--launch-arg").arg(arg);
        }
        let child = command
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", self.pgo.display()))?;
        Ok((child, out))
    }
}

/// `w.work_per_frame=N` from a `measure` output file.
fn score(out: &Path) -> std::result::Result<f64, String> {
    let text = std::fs::read_to_string(out).map_err(|e| e.to_string())?;
    text.lines()
        .find_map(|line| line.strip_prefix("w.work_per_frame="))
        .ok_or_else(|| format!("no w.work_per_frame in {}", out.display()))?
        .trim()
        .parse()
        .map_err(|e| format!("w.work_per_frame: {e}"))
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| Error(format!("git: {e}")))
}

/// The ticket: each file at `rev` and in the working tree.
pub(crate) fn read_ticket(repo: &Path, rev: &str, paths: &[String]) -> Result<Vec<TicketFile>> {
    let mut files = Vec::new();
    for path in paths {
        let spec = format!("{rev}:./{path}");
        let incumbent = if git(repo, &["cat-file", "-e", &spec])?.status.success() {
            let shown = git(repo, &["show", &spec])?;
            if !shown.status.success() {
                return Err(format!("git show {spec} failed").into());
            }
            Some(shown.stdout)
        } else {
            None
        };
        let candidate = match std::fs::read(repo.join(path)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("{path}: {error}").into()),
        };
        files.push(TicketFile {
            path: path.clone(),
            incumbent,
            candidate,
        });
    }
    Ok(files)
}

fn parse_polls(text: &str) -> Result<(u64, u64)> {
    let (from, to) = text
        .split_once("..")
        .ok_or_else(|| Error(format!("--polls wants FROM..TO, not {text:?}")))?;
    match (from.parse::<u64>(), to.parse::<u64>()) {
        (Ok(from), Ok(to)) if from < to => Ok((from, to)),
        _ => Err(format!("--polls wants FROM..TO with FROM < TO, not {text:?}").into()),
    }
}

/// `psoxide-perf adopt-pgo ...`: returns the process exit code.
///
/// # Errors
///
/// A malformed argument, a ticket file that cannot be read or written, or a build or bench
/// command that cannot be run.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let mut repo = None;
    let mut rev = "HEAD".to_string();
    let mut paths = Vec::new();
    let mut build = None;
    let mut image = None;
    let mut frontend = None;
    let mut tapes: Vec<(PathBuf, Option<(u64, u64)>)> = Vec::new();
    let mut launch_args = Vec::new();
    let mut offsets = DEFAULT_OFFSETS.to_vec();
    let mut tie_percent = DEFAULT_TIE_PERCENT;
    let mut jobs = 2;
    let mut timeout = 1800;
    let mut pgo = None;
    let mut work = None;
    let mut record_path = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .ok_or_else(|| Error(format!("{flag} wants a value")))
        };
        match flag.as_str() {
            "--repo" => repo = Some(PathBuf::from(value()?)),
            "--rev" => rev = value()?,
            "--file" => paths.push(value()?),
            "--build" => build = Some(value()?),
            "--image" => image = Some(PathBuf::from(value()?)),
            "--frontend" => frontend = Some(PathBuf::from(value()?)),
            "--tape" => tapes.push((PathBuf::from(value()?), None)),
            "--polls" => {
                let polls = parse_polls(&value()?)?;
                match tapes.last_mut() {
                    Some((_, slot @ None)) => *slot = Some(polls),
                    _ => return Err("each --polls follows its own --tape".into()),
                }
            }
            "--launch-arg" => launch_args.push(value()?),
            "--offsets" => {
                offsets = value()?
                    .split(',')
                    .map(|o| o.trim().parse::<i64>())
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|e| Error(format!("--offsets: {e}")))?;
            }
            "--tie" => {
                tie_percent = value()?.parse().map_err(|e| Error(format!("--tie: {e}")))?;
            }
            "--jobs" => {
                jobs = value()?
                    .parse()
                    .map_err(|e| Error(format!("--jobs: {e}")))?
            }
            "--timeout" => {
                timeout = value()?
                    .parse()
                    .map_err(|e| Error(format!("--timeout: {e}")))?;
            }
            "--pgo" => pgo = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--record" => record_path = Some(PathBuf::from(value()?)),
            "-h" | "--help" => {
                writeln!(out, "{HELP}")?;
                return Ok(0);
            }
            other => return Err(format!("adopt-pgo: unknown argument {other}\n{HELP}").into()),
        }
    }
    let missing = |what: &str| Error(format!("adopt-pgo needs {what}\n{HELP}"));
    let repo = repo.ok_or_else(|| missing("--repo"))?;
    let build = build.ok_or_else(|| missing("--build"))?;
    let image = image.ok_or_else(|| missing("--image"))?;
    let frontend = frontend.ok_or_else(|| missing("--frontend"))?;
    let record_path = record_path.ok_or_else(|| missing("--record"))?;
    if paths.is_empty() {
        return Err(missing("at least one --file"));
    }
    let mut windows: Vec<Window> = Vec::new();
    for (tape, polls) in tapes {
        let (from, to) = polls.ok_or_else(|| Error(format!("{} needs --polls", tape.display())))?;
        let name = tape
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .ok_or_else(|| Error(format!("{} has no file name", tape.display())))?;
        if windows.iter().any(|window| window.name == name) {
            return Err(format!("two windows are named {name} (one tape per window)").into());
        }
        windows.push(Window {
            name,
            tape,
            from,
            to,
        });
    }
    let pgo = match pgo {
        Some(pgo) => pgo,
        None => std::env::current_exe()?.with_file_name("psoxide-pgo"),
    };
    if !pgo.is_file() {
        return Err(format!(
            "no psoxide-pgo at {} (cargo build --release -p psoxide-pgo, or pass --pgo)",
            pgo.display()
        )
        .into());
    }
    let image = if image.is_absolute() {
        image
    } else {
        repo.join(image)
    };
    let work = work.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("psoxide-perf-adopt-{}", std::process::id()))
    });
    std::fs::create_dir_all(&work)?;
    let commit = git(
        &repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )?;
    if !commit.status.success() {
        return Err(format!("{rev} is not a commit of {}", repo.display()).into());
    }
    let provenance = Provenance {
        rev: String::from_utf8_lossy(&commit.stdout).trim().to_string(),
        frontend_sha256: sha256_hex(&std::fs::read(&frontend)?),
    };
    let files = read_ticket(&repo, &rev, &paths)?;
    let config = Config {
        windows,
        offsets,
        tie_percent,
    };
    let mut rig = CommandRig {
        root: repo.clone(),
        build,
        image,
        frontend,
        pgo,
        launch_args,
        jobs,
        timeout: Duration::from_secs(timeout),
        work: work.clone(),
    };
    let outcome = decide(&repo, &files, &config, &mut rig)?;
    let text = record(&outcome, &files, &config, &provenance);
    if let Some(parent) = record_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&record_path, &text)?;
    write!(out, "{text}")?;
    writeln!(
        out,
        "adopt-pgo: {} ({}); record {}, logs {}",
        outcome.decision.label(),
        outcome.reason,
        record_path.display(),
        work.display()
    )?;
    if outcome.candidate.is_some() {
        let winner = if outcome.decision == Decision::Adopt {
            Side::Candidate
        } else {
            Side::Incumbent
        };
        writeln!(
            out,
            "adopt-pgo: the {}'s files are in the tree and it was built last",
            winner.label()
        )?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scores keyed by side, window and offset; builds fail when asked.
    struct Fake {
        candidate: Vec<f64>,
        incumbent: Vec<f64>,
        fail_build: Option<Side>,
        built: Vec<Side>,
    }

    impl Fake {
        /// One mean per window for each side, the same at every offset.
        fn new(candidate: &[f64], incumbent: &[f64]) -> Self {
            Self {
                candidate: candidate.to_vec(),
                incumbent: incumbent.to_vec(),
                fail_build: None,
                built: Vec::new(),
            }
        }
    }

    impl Rig for Fake {
        fn build(&mut self, side: Side) -> std::result::Result<(), String> {
            self.built.push(side);
            if self.fail_build == Some(side) {
                return Err("profile does not bind".into());
            }
            Ok(())
        }
        fn bench(
            &mut self,
            side: Side,
            runs: &[(&Window, i64)],
        ) -> Vec<std::result::Result<f64, String>> {
            let means = match side {
                Side::Candidate => &self.candidate,
                Side::Incumbent => &self.incumbent,
            };
            runs.iter()
                .map(|(window, offset)| {
                    let index: usize = window.name[1..].parse().unwrap();
                    // Offsets wobble +-1 around the mean, which averages out.
                    Ok(means[index] + (*offset as f64).signum())
                })
                .collect()
        }
    }

    fn config(windows: usize) -> Config {
        Config {
            windows: (0..windows)
                .map(|i| Window {
                    name: format!("w{i}"),
                    tape: PathBuf::from(format!("w{i}.pxtape")),
                    from: 500,
                    to: 1800,
                })
                .collect(),
            offsets: vec![-24, 24],
            tie_percent: 0.25,
        }
    }

    fn ticket() -> Vec<TicketFile> {
        vec![
            TicketFile {
                path: "pgo/game.prof".into(),
                incumbent: Some(b"old profile".to_vec()),
                candidate: Some(b"new profile".to_vec()),
            },
            TicketFile {
                path: "pgo/hot-callsite-threshold".into(),
                incumbent: Some(b"800\n".to_vec()),
                candidate: Some(b"500\n".to_vec()),
            },
        ]
    }

    fn tree(files: &[TicketFile]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), files, Side::Candidate).unwrap();
        dir
    }

    fn in_tree(dir: &tempfile::TempDir) -> Vec<u8> {
        std::fs::read(dir.path().join("pgo/game.prof")).unwrap()
    }

    #[test]
    fn a_losing_candidate_is_rejected_and_the_incumbent_restored() {
        // Counter-Strike's shape: two maps a little faster, one 4.4% slower.
        let files = ticket();
        let dir = tree(&files);
        let mut rig = Fake::new(
            &[1_800_000.0, 1_890_000.0, 3_085_000.0],
            &[1_802_000.0, 1_897_000.0, 2_955_000.0],
        );
        let outcome = decide(dir.path(), &files, &config(3), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::KeepIncumbent);
        assert!(
            outcome.reason.contains("slower on w2 by +4.40%"),
            "{}",
            outcome.reason
        );
        assert_eq!(in_tree(&dir), b"old profile");
        assert_eq!(
            std::fs::read(dir.path().join("pgo/hot-callsite-threshold")).unwrap(),
            b"800\n"
        );
        // The incumbent was built last, so the image matches the files left in the tree.
        assert_eq!(rig.built, [Side::Candidate, Side::Incumbent]);
        // A win builds the candidate again: the winner is always the last build.
        let dir = tree(&files);
        let mut rig = Fake::new(&[900.0], &[1_000.0]);
        decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert_eq!(
            rig.built,
            [Side::Candidate, Side::Incumbent, Side::Candidate]
        );
    }

    #[test]
    fn a_winning_candidate_is_adopted() {
        let files = ticket();
        let dir = tree(&files);
        let mut rig = Fake::new(&[990.0, 1_985.0], &[1_000.0, 2_000.0]);
        let outcome = decide(dir.path(), &files, &config(2), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::Adopt, "{}", outcome.reason);
        assert_eq!(in_tree(&dir), b"new profile");
        assert!(outcome.geomean.unwrap() < 0.995);
    }

    #[test]
    fn a_tie_keeps_the_incumbent() {
        let files = ticket();
        let dir = tree(&files);
        // 0.1% faster: inside the 0.25% band.
        let mut rig = Fake::new(&[999.0, 1_998.0], &[1_000.0, 2_000.0]);
        let outcome = decide(dir.path(), &files, &config(2), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::KeepIncumbent);
        assert!(outcome.reason.contains("ties"), "{}", outcome.reason);
        assert_eq!(in_tree(&dir), b"old profile");
    }

    #[test]
    fn a_window_slower_inside_the_band_does_not_veto_a_win() {
        let files = ticket();
        let dir = tree(&files);
        let mut rig = Fake::new(&[1_002.0, 1_900.0], &[1_000.0, 2_000.0]);
        let outcome = decide(dir.path(), &files, &config(2), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::Adopt, "{}", outcome.reason);
    }

    #[test]
    fn a_candidate_that_does_not_build_is_rejected() {
        let files = ticket();
        let dir = tree(&files);
        let mut rig = Fake::new(&[1.0], &[1.0]);
        rig.fail_build = Some(Side::Candidate);
        let outcome = decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::KeepIncumbent);
        assert!(
            outcome.reason.contains("did not build"),
            "{}",
            outcome.reason
        );
        assert_eq!(in_tree(&dir), b"old profile");
    }

    #[test]
    fn an_incumbent_that_no_longer_builds_gives_way() {
        // After a repin a stale profile is refused by the build; it cannot be kept.
        let files = ticket();
        let dir = tree(&files);
        let mut rig = Fake::new(&[1_000.0], &[900.0]);
        rig.fail_build = Some(Side::Incumbent);
        let outcome = decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::Adopt);
        assert!(
            outcome.reason.contains("incumbent did not build"),
            "{}",
            outcome.reason
        );
        assert_eq!(in_tree(&dir), b"new profile");
    }

    #[test]
    fn no_incumbent_adopts_without_building_and_identical_tickets_do_nothing() {
        let mut files = ticket();
        for file in &mut files {
            file.incumbent = None;
        }
        let dir = tree(&files);
        let mut rig = Fake::new(&[], &[]);
        let outcome = decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::Adopt);
        assert!(rig.built.is_empty());
        for file in &mut files {
            file.incumbent = file.candidate.clone();
        }
        let outcome = decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert_eq!(outcome.decision, Decision::Unchanged);
        assert!(rig.built.is_empty());
    }

    #[test]
    fn a_file_absent_on_one_side_is_removed_when_that_side_is_in_the_tree() {
        let mut files = ticket();
        files.push(TicketFile {
            path: "pgo/game.layout".into(),
            incumbent: None,
            candidate: Some(b"layout".to_vec()),
        });
        let dir = tree(&files);
        let mut rig = Fake::new(&[1_100.0], &[1_000.0]);
        decide(dir.path(), &files, &config(1), &mut rig).unwrap();
        assert!(!dir.path().join("pgo/game.layout").exists());
    }

    #[test]
    fn the_record_holds_both_scores_and_the_decision_and_is_stable() {
        let files = ticket();
        let provenance = Provenance {
            rev: "b2ffb62279a3".into(),
            frontend_sha256: "00".into(),
        };
        let texts: Vec<String> = (0..2)
            .map(|_| {
                let dir = tree(&files);
                let mut rig = Fake::new(&[1_100.0, 2_000.0], &[1_000.0, 2_000.0]);
                let outcome = decide(dir.path(), &files, &config(2), &mut rig).unwrap();
                record(&outcome, &files, &config(2), &provenance)
            })
            .collect();
        assert_eq!(texts[0], texts[1]);
        let text = &texts[0];
        for line in [
            "decision keep-incumbent",
            "reason the candidate is slower on w0 by +10.00%",
            "rev b2ffb62279a3",
            "offsets -24 24",
            "score candidate w0 -24 1099",
            "score incumbent w0 24 1001",
            "mean candidate w0 1100.0",
            "mean incumbent w1 2000.0",
            "candidate_vs_incumbent w0 +10.00%",
            "candidate_vs_incumbent geomean +4.88%",
            "window w1 polls 500..1800",
        ] {
            assert!(
                text.lines().any(|l| l.starts_with(line)),
                "missing {line:?} in\n{text}"
            );
        }
        let hash = sha256_hex(b"old profile");
        assert!(text.contains(&format!("file pgo/game.prof incumbent {hash} candidate ")));
    }

    #[test]
    fn polls_and_scores_parse() {
        assert_eq!(parse_polls("500..1950").unwrap(), (500, 1950));
        assert!(parse_polls("1950..500").is_err());
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("m.out");
        std::fs::write(&out, "w.flips=665\nw.work_per_frame=3111665\n").unwrap();
        assert_eq!(score(&out).unwrap(), 3_111_665.0);
        std::fs::write(&out, "w.flips=665\n").unwrap();
        assert!(score(&out).is_err());
    }
}
