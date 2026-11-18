//! The verdict: checks, stage results, the content-addressed result file and the human table.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util;

/// Passing checks the table prints however large the stage: summaries a reader must see (results
/// carried from an older gate, the hardware-renderer coverage, the open-loop total).
const ALWAYS_SHOWN: &[&str] = &[
    "carried from an older gate",
    "journey rows",
    "hardware renderer (hw = true)",
    "open-loop steps",
];

/// One check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

/// One check: what was measured, against what, and the verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Check {
    pub name: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub baseline: String,
    /// Why it failed or was skipped, or where the evidence is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Check {
    pub fn pass(name: impl Into<String>, value: impl Into<String>) -> Check {
        Check::new(name, Status::Pass, value)
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Check {
        Check::new(name, Status::Fail, "").detail(detail)
    }
    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Check {
        Check::new(name, Status::Skip, "").detail(reason)
    }
    pub fn new(name: impl Into<String>, status: Status, value: impl Into<String>) -> Check {
        Check {
            name: name.into(),
            status,
            value: value.into(),
            baseline: String::new(),
            detail: String::new(),
        }
    }
    pub fn baseline(mut self, baseline: impl Into<String>) -> Check {
        self.baseline = baseline.into();
        self
    }
    pub fn detail(mut self, detail: impl Into<String>) -> Check {
        self.detail = detail.into();
        self
    }
}

/// What a stage produced: its checks and named facts later stages read (artifact digests,
/// the SDK pin, measured values for `bless`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub checks: Vec<Check>,
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.status != Status::Fail)
    }
    pub fn error(name: &str, message: impl Into<String>) -> Outcome {
        Outcome {
            checks: vec![Check::fail(name, message)],
            facts: BTreeMap::new(),
        }
    }
}

/// How a stage's outcome was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Ran now.
    Run,
    /// Reused from a receipt with the same key.
    Hit,
    /// Partly reused: some jobs ran, some hit (journeys, speed).
    Mixed,
    /// Did not run: an earlier stage it needs failed.
    NotRun,
}

/// One stage in the result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub name: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    pub outcome: Outcome,
}

impl StageResult {
    pub fn passed(&self) -> bool {
        self.source != Source::NotRun && self.outcome.passed()
    }
}

/// The run's result file. Its name is the SHA-256 of its JSON, so the same bytes judged the same
/// way give the same name; wall times live in a sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub schema: u32,
    pub game: String,
    pub commit: String,
    pub dirty: bool,
    pub sdk_pin: String,
    /// The SDK pin rule's line: "guest tree equals main <sha12>" or why not.
    #[serde(default)]
    pub sdk_guest: String,
    pub harness: String,
    pub gate_revision: String,
    pub tier: String,
    pub stages: Vec<StageResult>,
    /// Every skipped check, `stage: check: reason`.
    pub skips: Vec<String>,
    pub verdict: Status,
    /// Required pieces of a landing verdict that this run does not hold; empty = complete.
    pub landing_missing: Vec<String>,
}

impl RunResult {
    /// Fill `skips` and `verdict` from the stages.
    pub fn finish(&mut self) {
        self.skips = self
            .stages
            .iter()
            .flat_map(|s| {
                s.outcome
                    .checks
                    .iter()
                    .filter(|c| c.status == Status::Skip)
                    .map(move |c| format!("{}: {}: {}", s.name, c.name, c.detail))
            })
            .collect();
        let failed = self
            .stages
            .iter()
            .any(|s| s.source == Source::NotRun || !s.outcome.passed());
        self.verdict = if failed { Status::Fail } else { Status::Pass };
    }

    /// Write `<dir>/<sha256>.json` and the timing sidecar, and point `latest` at it.
    pub fn write(&self, dir: &Path, timing: &[Timing]) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let hash = util::sha256_bytes(json.as_bytes());
        let path = dir.join(format!("{hash}.json"));
        std::fs::write(&path, &json).map_err(|e| format!("write {}: {e}", path.display()))?;
        let timing_json = serde_json::to_string_pretty(timing).map_err(|e| e.to_string())?;
        let timing_path = dir.join(format!("{hash}.timing.json"));
        std::fs::write(&timing_path, timing_json)
            .map_err(|e| format!("write {}: {e}", timing_path.display()))?;
        std::fs::write(dir.join("latest"), format!("{hash}\n"))
            .map_err(|e| format!("write latest: {e}"))?;
        Ok(path)
    }

    /// The human table.
    pub fn table(&self, timing: &[Timing]) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\n{} at {}{} (SDK {}{}, gate {}, tier {})",
            self.game,
            util::short(&self.commit),
            if self.dirty { " +dirty" } else { "" },
            util::short(&self.sdk_pin),
            if self.sdk_guest.is_empty() {
                String::new()
            } else {
                format!(", {}", self.sdk_guest)
            },
            util::short(&self.gate_revision),
            self.tier
        );
        for stage in &self.stages {
            let wall = timing
                .iter()
                .find(|t| t.what == stage.name)
                .map_or(String::new(), |t| format!("{:.0} s", t.end - t.start));
            let pass = stage
                .outcome
                .checks
                .iter()
                .filter(|c| c.status == Status::Pass)
                .count();
            let fail = stage
                .outcome
                .checks
                .iter()
                .filter(|c| c.status == Status::Fail)
                .count();
            let skip = stage
                .outcome
                .checks
                .iter()
                .filter(|c| c.status == Status::Skip)
                .count();
            let verdict = match (stage.source, stage.outcome.passed()) {
                (Source::NotRun, _) => "NOT RUN",
                (_, true) => "PASS",
                (_, false) => "FAIL",
            };
            let source = match stage.source {
                Source::Run => "run",
                Source::Hit => "receipt",
                Source::Mixed => "run+receipt",
                Source::NotRun => "-",
            };
            let _ = writeln!(
                out,
                "  {:<9} {:<8} {:>4} pass {:>3} fail {:>3} skip  {:<11} {:>7}",
                stage.name, verdict, pass, fail, skip, source, wall
            );
            for c in &stage.outcome.checks {
                let summary = ALWAYS_SHOWN.iter().any(|p| c.name.starts_with(p));
                if c.status != Status::Pass || stage.outcome.checks.len() <= 8 || summary {
                    let status = match c.status {
                        Status::Pass => "pass",
                        Status::Fail => "FAIL",
                        Status::Skip => "skip",
                    };
                    let mut line = format!("      {status} {}", c.name);
                    if !c.value.is_empty() {
                        let _ = write!(line, "  {}", c.value);
                    }
                    if !c.baseline.is_empty() {
                        let _ = write!(line, "  (baseline {})", c.baseline);
                    }
                    // A red stage shows every check's detail: a passing check's detail there is
                    // the explanation of the red (the size stage's text by function).
                    if !c.detail.is_empty() && (c.status != Status::Pass || !stage.outcome.passed())
                    {
                        let indent = "\n          ";
                        let _ = write!(line, "  {}", c.detail.replace('\n', indent));
                    }
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        // Every skip is counted, by stage, so none hides in a long table.
        let mut by_stage: Vec<String> = Vec::new();
        for stage in &self.stages {
            let n = stage
                .outcome
                .checks
                .iter()
                .filter(|c| c.status == Status::Skip)
                .count();
            if n > 0 {
                by_stage.push(format!("{} {n}", stage.name));
            }
        }
        let _ = writeln!(
            out,
            "  skips: {}{}",
            self.skips.len(),
            if by_stage.is_empty() {
                String::new()
            } else {
                format!(" ({})", by_stage.join(", "))
            }
        );
        let _ = writeln!(
            out,
            "VERDICT {}",
            if self.verdict == Status::Pass {
                "PASS"
            } else {
                "FAIL"
            }
        );
        if self.landing_missing.is_empty() && self.verdict == Status::Pass {
            let _ = writeln!(
                out,
                "LANDING: COMPLETE PASS (every required tier ran on these bytes and passed)"
            );
        } else if self.landing_missing.is_empty() {
            let _ = writeln!(
                out,
                "LANDING: REFUSED (every required tier ran on these bytes; the verdict is FAIL)"
            );
        } else {
            let _ = writeln!(
                out,
                "NOT A LANDING VERDICT: missing {}",
                self.landing_missing.join(", ")
            );
        }
        out
    }
}

/// Start and end of one stage or job, for the timing sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timing {
    pub what: String,
    pub start: f64,
    pub end: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(stages: Vec<StageResult>) -> RunResult {
        let mut r = RunResult {
            schema: 1,
            game: "g".into(),
            commit: "c".repeat(40),
            dirty: false,
            sdk_pin: "s".repeat(40),
            sdk_guest: String::new(),
            harness: "0.1.0".into(),
            gate_revision: "r".repeat(40),
            tier: "quick".into(),
            stages,
            skips: Vec::new(),
            verdict: Status::Pass,
            landing_missing: vec!["full tier".into()],
        };
        r.finish();
        r
    }

    fn stage(name: &str, checks: Vec<Check>, source: Source) -> StageResult {
        StageResult {
            name: name.into(),
            source,
            key: String::new(),
            outcome: Outcome {
                checks,
                facts: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn a_skip_is_counted_and_does_not_fail() {
        let r = result(vec![stage(
            "journeys",
            vec![Check::pass("a", ""), Check::skip("b", "golden_on")],
            Source::Run,
        )]);
        assert_eq!(r.verdict, Status::Pass);
        assert_eq!(r.skips, ["journeys: b: golden_on"]);
    }

    #[test]
    fn a_stage_that_did_not_run_fails_the_verdict() {
        let r = result(vec![stage("size", Vec::new(), Source::NotRun)]);
        assert_eq!(r.verdict, Status::Fail);
        assert!(r.table(&[]).contains("NOT RUN"));
    }

    #[test]
    fn the_result_name_is_its_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        let r = result(vec![stage(
            "size",
            vec![Check::pass("text", "1")],
            Source::Run,
        )]);
        let a = r.write(dir.path(), &[]).unwrap();
        let b = r
            .write(
                dir.path(),
                &[Timing {
                    what: "size".into(),
                    start: 1.0,
                    end: 2.0,
                }],
            )
            .unwrap();
        assert_eq!(a, b, "wall times never change the result name");
    }
}
