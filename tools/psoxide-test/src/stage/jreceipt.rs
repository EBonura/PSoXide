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
        }
    }

    /// The same run: what the gate measured is identical (the runner and provenance aside).
    pub fn same_run(&self, other: &Record) -> bool {
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
                key.file(&file.display().to_string(), &file)?;
            }
        }
        Ok(Shared {
            symbols,
            fixtures: key.finish(),
        })
    }
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
    discs: &[String],
    golden: &Path,
    flags: &[String],
) -> Result<String, String> {
    let mut key = Key::new("journey-content");
    key.file("journey", journey)?;
    for disc in discs {
        let cue = ctx.repo.join(disc);
        key.file("cue", &cue)?;
        key.file("bin", &cue.with_extension("bin"))?;
    }
    for (build, digest) in &shared.symbols {
        key.part(&format!("symbols {build}"), digest);
    }
    key.part("fixtures", &shared.fixtures);
    for file in files_under(golden, &|_| true) {
        key.file(&file.display().to_string(), &file)?;
    }
    // Flags carry the renderer, the conformance rules and a golden directory path; the path is
    // machine-local, its content is keyed above.
    for f in flags {
        key.part("flag", f);
    }
    key.part("check-gpu-ll-fifo", "1");
    Ok(key.finish())
}

/// The receipts root of a game: `<work>/<game>/journey-receipts`.
pub fn root(ctx: &Ctx<'_>) -> PathBuf {
    ctx.work.join("journey-receipts")
}

/// Where a record lives: `<root>/<key>/<runner12>@<model>.json`.
pub fn path(root: &Path, key: &str, runner: &str, model: &str) -> PathBuf {
    root.join(key).join(format!(
        "{}@{}.json",
        util::short(runner),
        model.replace([',', '='], "-")
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

/// A result for `model` to carry to `runner`: an older revision's record for this model whose
/// `real` record under that revision is the same run as `real` under `runner`.
pub fn carry(root: &Path, key: &str, runner: &str, model: &str) -> Option<Record> {
    let now_real = load(&path(root, key, runner, "real"))?;
    let dir = root.join(key);
    let suffix = format!("@{}.json", model.replace([',', '='], "-"));
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
        if then_real.carried_from.is_none() && then_real.same_run(&now_real) {
            return Some(Record {
                runner: runner.to_string(),
                carried_from: Some(record.runner.clone()),
                ..record
            });
        }
    }
    None
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
        }
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
    }

    #[test]
    fn a_record_round_trips_as_a_receipt_line() {
        let r = record("a", 7);
        let line = r.line();
        assert!(line.passed && line.receipt_hit && !line.skips_unknown);
        assert_eq!(Record::of("a", &line), r);
    }
}
