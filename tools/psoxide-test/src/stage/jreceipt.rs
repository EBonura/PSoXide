//! The harness's own journey receipts, keyed by what a gate run reads, not by where the bytes came
//! from: the journey file, the disc it boots, the symbol table of every build (addresses and
//! names, never the map's object paths, which change with the SDK checkout even when the code
//! does not), the fixtures beside the journeys, its goldens, the model and the gate flags. A pin
//! move that rebuilds identical bytes hits every one of them.
//!
//! The runner is the other half. A receipt is stored per gate revision. When the pinned gate
//! changes, nothing is trusted on the old revision's word alone: every journey runs again on
//! `real`, and a result on another model is carried forward only when that `real` run reproduced
//! the old revision's `real` result exactly (verdict, every check count, ticks, conformance lines).
//! A carried result is printed and counted as carried, never as a fresh PASS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::stage::journeys::GateLine;
use crate::stage::Ctx;
use crate::util::{self, Key};

/// What a passing gate run is remembered by.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    /// The gate revision that produced (or carried) it.
    pub runner: String,
    pub ticks: u64,
    pub checks: u64,
    pub pass: u64,
    pub fail: u64,
    pub skip: u64,
    pub golden_skipped: u64,
    pub conformance: Vec<String>,
    #[serde(default)]
    pub hw: Option<(u64, u64)>,
    /// Set when this result was carried from an older gate revision, not run on this one.
    #[serde(default)]
    pub carried_from: Option<String>,
    /// The gate answered from its own receipt, whose summary line has fewer counts than a run:
    /// such a record is a valid result but never the evidence for carrying another model.
    #[serde(default)]
    pub gate_receipt: bool,
    /// The behaviour areas the run exercised and their versions (the gate's `AREAS` line;
    /// empty from a gate that does not print it).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub areas: BTreeMap<String, String>,
    /// What the run did (the gate's `DIGEST` line), when the gate prints it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// What the run met of the events the hardware models treat differently (`SIGNALS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<super::equiv::Signals>,
}

impl Record {
    /// A record of a passing gate line.
    pub fn of(runner: &str, line: &GateLine) -> Record {
        Record {
            runner: runner.to_string(),
            ticks: line.ticks,
            checks: line.checks,
            pass: line.pass,
            fail: line.fail,
            skip: line.skip,
            golden_skipped: line.golden_skipped,
            conformance: line.conformance.clone(),
            hw: line.hw,
            carried_from: None,
            gate_receipt: line.receipt_hit,
            areas: line.areas.clone(),
            digest: line.digest.clone(),
            signals: line.signals.clone(),
        }
    }

    /// The gate line it stands for (a receipt hit with every count known).
    pub fn line(&self) -> GateLine {
        GateLine {
            passed: true,
            checks: self.checks,
            pass: self.pass,
            fail: self.fail,
            skip: self.skip,
            golden_skipped: self.golden_skipped,
            ticks: self.ticks,
            receipt_hit: true,
            conformance: self.conformance.clone(),
            aborts: Vec::new(),
            skips_unknown: false,
            hw: self.hw,
            carried_from: self.carried_from.clone(),
            harness_hit: true,
            failures: Vec::new(),
            areas: self.areas.clone(),
            digest: self.digest.clone(),
            signals: self.signals.clone(),
            stands_on: None,
        }
    }

    /// The same run: what the gate measured is identical (the runner and provenance aside); when
    /// both gates print a run digest, the digests must agree too.
    pub fn same_run(&self, other: &Record) -> bool {
        if let (Some(a), Some(b)) = (&self.digest, &other.digest) {
            if a != b {
                return false;
            }
        }
        (
            self.ticks,
            self.checks,
            self.pass,
            self.fail,
            self.skip,
            self.golden_skipped,
            &self.conformance,
        ) == (
            other.ticks,
            other.checks,
            other.pass,
            other.fail,
            other.skip,
            other.golden_skipped,
            &other.conformance,
        )
    }
}

/// Digests the content key reads that are the same for every journey of one stage run.
pub struct Shared {
    /// Per build with a map: the digest of its symbol list (or of the map when there is none).
    symbols: BTreeMap<String, String>,
    /// Every file beside the journeys that is not a journey or a golden: tapes, cards, fixtures.
    fixtures: String,
}

impl Shared {
    /// Read them once.
    pub fn read(ctx: &Ctx<'_>, journeys: &[PathBuf]) -> Result<Shared, String> {
        let mut symbols = BTreeMap::new();
        for b in &ctx.game.builds {
            let list = b.out_dir(&ctx.repo).join(super::build::SYMBOLS_FILE);
            let source = if list.is_file() {
                Some(list)
            } else {
                b.map_path(&ctx.repo)
            };
            if let Some(path) = source.filter(|p| p.is_file()) {
                symbols.insert(b.name.clone(), util::sha256_file(&path)?);
            }
            // Only a build with `images` has this list, so every other key is unchanged.
            let images = b.out_dir(&ctx.repo).join(super::build::IMAGE_SYMBOLS);
            if !b.images.is_empty() && images.is_file() {
                symbols.insert(format!("{}:images", b.name), util::sha256_file(&images)?);
            }
        }
        let mut dirs: Vec<PathBuf> = journeys
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        dirs.sort();
        dirs.dedup();
        let mut key = Key::new("journey-fixtures");
        for dir in dirs {
            for file in files_under(&dir, &|rel: &str| {
                !(rel.starts_with("golden/") || rel.ends_with(".toml") && !rel.contains('/'))
            }) {
                key.file(&relative(&ctx.repo, &file), &file)?;
            }
        }
        Ok(Shared {
            symbols,
            fixtures: key.finish(),
        })
    }
}

/// `path` relative to the repo, so a key does not depend on where the clone lives.
fn relative(repo: &Path, path: &Path) -> String {
    util::relative_display(repo, path)
}

/// Every file under `dir` whose path relative to it passes `keep`, sorted.
fn files_under(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(dir)
                    .map(|r| r.display().to_string())
                    .unwrap_or_default();
                if keep(&rel) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// The content key of one journey under one set of gate flags (the model is not in it: records
/// are stored per model under it).
pub fn content_key(
    ctx: &Ctx<'_>,
    shared: &Shared,
    journey: &Path,
    instance: Option<&str>,
    discs: &[String],
    golden: &Path,
    flags: &[String],
) -> Result<String, String> {
    let mut key = Key::new("journey-content");
    // A generated instance names the repo absolutely: key it on its template and member values.
    match instance {
        Some(text) => key.part("journey instance", text),
        None => key.file("journey", journey)?,
    }
    for disc in discs {
        let cue = ctx.repo.join(disc);
        for (label, file) in [("cue", cue.clone()), ("bin", cue.with_extension("bin"))] {
            // The build stage already hashed the files it wrote; hash only what it did not.
            match built_digest(ctx, &file) {
                Some(d) => key.part(label, &d),
                None => key.file(label, &file)?,
            }
        }
    }
    for (build, digest) in &shared.symbols {
        key.part(&format!("symbols {build}"), digest);
    }
    key.part("fixtures", &shared.fixtures);
    for file in files_under(golden, &|_| true) {
        key.file(&relative(&ctx.repo, &file), &file)?;
    }
    // Flags carry the renderer, the conformance rules and a golden directory path; the path is
    // machine-local, its content is keyed above.
    for f in flags {
        key.part("flag", &relative(&ctx.repo, Path::new(f)));
    }
    key.part("check-gpu-ll-fifo", "1");
    Ok(key.finish())
}

/// The receipts root of a game: `<work>/<game>/journey-receipts`.
pub fn root(ctx: &Ctx<'_>) -> PathBuf {
    ctx.work.join("journey-receipts")
}

/// The digest the build stage recorded for `file`, when a build of this run wrote it.
fn built_digest(ctx: &Ctx<'_>, file: &Path) -> Option<String> {
    let dir = file.parent()?;
    let name = file.file_name()?.to_string_lossy().into_owned();
    ctx.game
        .builds
        .iter()
        .find(|b| b.out_dir(&ctx.repo) == dir)
        .and_then(|b| ctx.artifacts.get(&b.name))
        .and_then(|files| files.get(&name))
        .cloned()
}

/// Where a record lives: `<root>/<key>/<runner12>@<model>.json`.
pub fn path(root: &Path, key: &str, runner: &str, model: &str) -> PathBuf {
    root.join(key).join(format!(
        "{}@{}.json",
        util::short(runner),
        model.replace([',', '=', '/'], "-")
    ))
}

/// The record at `path`, if any.
pub fn load(path: &Path) -> Option<Record> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Store a record.
pub fn store(path: &Path, record: &Record) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(record) {
        let _ = std::fs::write(path, json);
    }
}

/// Whether an older gate revision left a record for `model` under `key` (then the new gate's run
/// must be a full one, not the gate's own receipt, to serve as the evidence for carrying).
pub fn older_exists(root: &Path, key: &str, runner: &str, model: &str) -> bool {
    let suffix = format!("@{}.json", model.replace([',', '=', '/'], "-"));
    let current = format!("{}{suffix}", util::short(runner));
    std::fs::read_dir(root.join(key)).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(&suffix) && n != current
        })
    })
}

/// A result for `model` to carry to `runner`: an older revision's record for this model whose
/// `real` record under that revision is the same run as `real` under `runner`.
pub fn carry(root: &Path, key: &str, runner: &str, model: &str) -> Option<Record> {
    let now_real = load(&path(root, key, runner, "real"))?;
    let dir = root.join(key);
    let suffix = format!("@{}.json", model.replace([',', '=', '/'], "-"));
    let mut old: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
        .collect();
    old.sort();
    for candidate in old {
        let Some(record) = load(&candidate) else {
            continue;
        };
        if record.runner == runner || record.carried_from.is_some() {
            continue;
        }
        let Some(then_real) = load(&path(root, key, &record.runner, "real")) else {
            continue;
        };
        let full_runs = !then_real.gate_receipt && !now_real.gate_receipt;
        if full_runs && then_real.carried_from.is_none() && then_real.same_run(&now_real) {
            return Some(Record {
                runner: runner.to_string(),
                carried_from: Some(record.runner.clone()),
                ..record
            });
        }
    }
    None
}

/// The behaviour versions a gate reports (`psoxide-gate versions`: `VERSIONS cpu=1 ... rules=<hex>
/// journey=1`), or `None` when it has none (a gate from before behaviour versions).
pub fn parse_versions(output: &str) -> Option<BTreeMap<String, String>> {
    let line = output.lines().find_map(|l| l.strip_prefix("VERSIONS "))?;
    let map: BTreeMap<String, String> = line
        .split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    (!map.is_empty()).then_some(map)
}

/// A result for `model` to carry to `runner` by behaviour areas: an older revision's own run (not
/// a carry, not the gate's receipt) whose every exercised area has the same version on the new
/// gate. No `real` re-proof is needed: the run depended on nothing the new gate changed.
pub fn carry_by_areas(
    root: &Path,
    key: &str,
    runner: &str,
    model: &str,
    versions: &BTreeMap<String, String>,
) -> Option<Record> {
    let suffix = format!("@{}.json", model.replace([',', '=', '/'], "-"));
    let mut old: Vec<PathBuf> = std::fs::read_dir(root.join(key))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
        .collect();
    old.sort();
    old.into_iter()
        .filter_map(|p| load(&p))
        .find(|r| {
            r.runner != runner
                && r.carried_from.is_none()
                && !r.gate_receipt
                && !r.areas.is_empty()
                && r.areas.iter().all(|(a, v)| versions.get(a) == Some(v))
        })
        .map(|record| Record {
            runner: runner.to_string(),
            carried_from: Some(record.runner.clone()),
            ..record
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(runner: &str, ticks: u64) -> Record {
        Record {
            runner: runner.into(),
            ticks,
            checks: 10,
            pass: 10,
            fail: 0,
            skip: 0,
            golden_skipped: 0,
            conformance: vec!["CONFORMANCE j gpu-irq-command=5".into()],
            hw: None,
            carried_from: None,
            gate_receipt: false,
            areas: BTreeMap::new(),
            digest: None,
            signals: None,
        }
    }

    #[test]
    fn a_signals_line_survives_the_receipt() {
        let out = format!(
            "{CAPTURED}SIGNALS nitroxide-first-to-one real gp0-1f=0 gp0-1f-written=0 \
             cd-ack-broken=0 mdec-commands=0 golden-on=no\n"
        );
        let line = crate::stage::journeys::parse_gate(&out).unwrap();
        let signals = line.signals.clone().expect("SIGNALS parsed");
        assert_eq!(signals, crate::stage::equiv::Signals::default());
        assert_eq!(Record::of("g", &line).line().signals, Some(signals));
        // The older gate printed none: nothing can stand on its rows.
        let old = crate::stage::journeys::parse_gate(CAPTURED).unwrap();
        assert_eq!(old.signals, None);
    }

    /// Lines the emu-gate branch prints (captured from its fleet sample, lane/gate-batch 076d97fb).
    const CAPTURED: &str = "PASS nitroxide-first-to-one 12 checks: 12 pass, 0 fail | 640 ticks (10 s emulated) in 1.0 s | disc ab\n\
AREAS nitroxide-first-to-one real cpu=1 gte=1 muldiv=1 cd=1 spu=1 pad=1 gpu=2 dma=1 timers=1 irq=1 card=1 hw=2 rules=98a2b1108be61124 journey=1\n\
DIGEST nitroxide-first-to-one real 63c51fd331310056c1fcfec1b84725c5\n";

    #[test]
    fn areas_and_the_digest_are_read_from_the_gate() {
        let line = crate::stage::journeys::parse_gate(CAPTURED).unwrap();
        assert_eq!(line.areas.get("gpu").map(String::as_str), Some("2"));
        assert_eq!(
            line.areas.get("rules").map(String::as_str),
            Some("98a2b1108be61124")
        );
        assert!(!line.areas.contains_key("mdec"));
        assert_eq!(
            line.digest.as_deref(),
            Some("63c51fd331310056c1fcfec1b84725c5")
        );
        // A model with `=` in it is not an area.
        let never = CAPTURED.replace(" real ", " mister,gpu-irq=never ");
        let line = crate::stage::journeys::parse_gate(&never).unwrap();
        assert!(!line.areas.keys().any(|k| k.contains("mister")));
        let v = parse_versions("VERSIONS cpu=1 gte=1 gpu=3 rules=98a2b1108be61124 journey=1\n")
            .unwrap();
        assert_eq!(v.get("gpu").map(String::as_str), Some("3"));
        assert!(parse_versions("error: unrecognized subcommand 'versions'").is_none());
    }

    #[test]
    fn a_row_is_carried_when_every_area_it_exercised_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (old, new) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        let line = crate::stage::journeys::parse_gate(CAPTURED).unwrap();
        store(&path(root, "k", old, "real"), &Record::of(old, &line));
        let mut versions = line.areas.clone();
        versions.insert("mdec".into(), "7".into()); // an area the run did not exercise moved
        let carried = carry_by_areas(root, "k", new, "real", &versions).unwrap();
        assert_eq!(carried.carried_from.as_deref(), Some(old));
        versions.insert("gpu".into(), "3".into()); // one it did exercise moved
        assert!(carry_by_areas(root, "k", new, "real", &versions).is_none());
        // A record from a gate without areas is never carried this way.
        store(&path(root, "k2", old, "real"), &record(old, 5));
        assert!(carry_by_areas(root, "k2", new, "real", &line.areas).is_none());
        // Digests that differ make two runs different even with equal counts.
        let mut a = Record::of(old, &line);
        let mut b = a.clone();
        b.digest = Some("00".into());
        assert!(!a.same_run(&b));
        a.digest = None;
        assert!(a.same_run(&b));
    }

    #[test]
    fn the_same_run_ignores_the_runner_and_nothing_else() {
        assert!(record("a", 100).same_run(&record("b", 100)));
        assert!(!record("a", 100).same_run(&record("a", 101)));
        let mut other = record("b", 100);
        other
            .conformance
            .push("CONFORMANCE j mdec-status-before-feed=1".into());
        assert!(!record("a", 100).same_run(&other));
    }

    #[test]
    fn a_model_is_carried_only_when_real_reproduced_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (old, new) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        store(&path(root, "k", old, "real"), &record(old, 100));
        store(&path(root, "k", old, "mister"), &record(old, 120));
        // Nothing on the new gate yet: no carry.
        assert!(carry(root, "k", new, "mister").is_none());
        // The new gate's real run differs: no carry.
        store(&path(root, "k", new, "real"), &record(new, 101));
        assert!(carry(root, "k", new, "mister").is_none());
        // It reproduces: mister is carried, marked as such, under the new runner.
        store(&path(root, "k", new, "real"), &record(new, 100));
        let carried = carry(root, "k", new, "mister").unwrap();
        assert_eq!(carried.carried_from.as_deref(), Some(old));
        assert_eq!(carried.runner, new);
        assert_eq!(carried.ticks, 120);
        // A model never recorded is not invented.
        assert!(carry(root, "k", new, "mister,gpu-irq=never").is_none());
        // The gate's own receipt is not evidence: its summary is shorter than a run's.
        let mut short = record(new, 100);
        short.gate_receipt = true;
        store(&path(root, "k", new, "real"), &short);
        assert!(carry(root, "k", new, "mister").is_none());
        assert!(older_exists(root, "k", new, "mister"));
        assert!(!older_exists(root, "k", old, "mister,gpu-irq=never"));
    }

    #[test]
    fn a_record_round_trips_as_a_receipt_line() {
        let r = record("a", 7);
        let line = r.line();
        assert!(line.passed && line.receipt_hit && !line.skips_unknown);
        assert!(Record::of("a", &line).same_run(&r));
    }
}
