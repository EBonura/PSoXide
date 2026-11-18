//! Journeys on PSoXide: the pinned gate is the one runner. The harness lints every journey first
//! (explicit pad, the open-loop ratchet, a disc that a declared build produced), then runs one
//! gate process per journey and model, longest first, inside the emulator budget, with the
//! gate's own content receipts and the game's conformance rules.

use std::path::{Path, PathBuf};

use crate::journey::{self, Info};
use crate::report::{Check, Outcome, Source, StageResult, Status};
use crate::stage::{Ctx, Tier, FULL_MODELS};
use crate::{sched, util};

/// What one gate run printed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GateLine {
    pub passed: bool,
    pub checks: u64,
    pub pass: u64,
    pub fail: u64,
    pub skip: u64,
    pub golden_skipped: u64,
    pub ticks: u64,
    pub receipt_hit: bool,
    pub conformance: Vec<String>,
    pub aborts: Vec<String>,
    /// A receipt hit whose skip counts this harness never saw: the gate's hit line has none.
    pub skips_unknown: bool,
}

/// Read the gate's summary, receipt, conformance and abort lines.
pub fn parse_gate(output: &str) -> Option<GateLine> {
    let mut line = GateLine::default();
    let mut found = false;
    for l in output.lines() {
        if l.starts_with("RECEIPT HIT ") {
            line.receipt_hit = true;
        } else if l.starts_with("CONFORMANCE ") {
            line.conformance.push(l.to_string());
        } else if let Some(rest) = l.trim_start().strip_prefix("ABORT ") {
            line.aborts.push(rest.trim().to_string());
        } else if (l.starts_with("PASS ") || l.starts_with("FAIL ")) && l.contains(" checks: ") {
            found = true;
            line.passed = l.starts_with("PASS ");
            let (head, tail) = l.split_once(" | ").unwrap_or((l, ""));
            let counts = head.split_once(" checks: ").map_or("", |(_, c)| c);
            line.checks = head
                .split_once(" checks: ")
                .and_then(|(h, _)| h.rsplit(' ').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            for part in counts.split(", ") {
                let (n, word) = part.split_once(' ').unwrap_or(("0", part));
                let n: u64 = n.parse().unwrap_or(0);
                match word {
                    "pass" => line.pass = n,
                    "fail" => line.fail = n,
                    "skip" => line.skip = n,
                    w if w.starts_with("golden skipped") => line.golden_skipped = n,
                    _ => {}
                }
            }
            line.ticks = tail
                .split_once(" ticks")
                .and_then(|(t, _)| t.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    found.then_some(line)
}

/// The gate answers a receipt hit with a short line that has no skip counts, so the harness keeps
/// what each fresh run skipped (golden_on compares, unenforced conformance rules) under a key of
/// the same inputs, and puts it back on a hit. A hit it never saw run is reported as unknown.
fn remember_skips(ctx: &Ctx<'_>, gate: &Path, job: &Job, conformance: &str, line: &mut GateLine) {
    let mut key = crate::util::Key::new("journey-skips");
    let _ = key.file("journey", &job.info.path);
    if let Some(disc) = job.info.discs.first() {
        let cue = ctx.repo.join(disc);
        let _ = key.file("disc", &cue.with_extension("bin"));
    }
    let _ = key.file("gate", gate);
    key.part("model", &job.model);
    key.part("conformance", conformance);
    let path = ctx
        .work
        .join("journeys")
        .join("skips")
        .join(format!("{}.json", key.finish()));
    if line.receipt_hit {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<(u64, u64, Vec<String>)>(&t).ok())
        {
            Some((skip, golden, conformance)) => {
                line.skip = skip;
                line.golden_skipped = golden;
                line.conformance = conformance;
            }
            None => line.skips_unknown = true,
        }
    } else if line.passed {
        let record = (line.skip, line.golden_skipped, line.conformance.clone());
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&record) {
            let _ = std::fs::write(&path, json);
        }
    }
}

/// The rule names a CONFORMANCE line marks `[not enforced]`, comma-separated.
fn not_enforced(lines: &[String]) -> String {
    let names: Vec<String> = lines
        .iter()
        .flat_map(|l| l.split("; "))
        .filter(|rule| rule.contains("[not enforced]"))
        .filter_map(|rule| rule.split('=').next())
        .filter_map(|head| head.rsplit(' ').next())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        "see the gate log".into()
    } else {
        names.join(", ")
    }
}

/// One (journey, model) job.
struct Job {
    info: Info,
    model: String,
    expected_ticks: u64,
}

/// The journeys stage. `bless` runs every journey on `real` and does not enforce the ratchet.
pub fn run(ctx: &Ctx<'_>, bless: bool) -> StageResult {
    let result = |source, outcome| StageResult {
        name: "journeys".into(),
        source,
        key: String::new(),
        outcome,
    };
    let Some(spec) = &ctx.game.journeys else {
        return result(
            Source::Run,
            Outcome {
                checks: vec![Check::skip("journeys", "the game file has no [journeys]")],
                ..Outcome::default()
            },
        );
    };
    let gate = match ctx.config.gate() {
        Ok(g) => g.to_path_buf(),
        Err(e) => return result(Source::Run, Outcome::error("gate", e)),
    };
    let paths = match util::glob(&ctx.repo, &spec.files) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            return result(
                Source::Run,
                Outcome::error("journeys", format!("no file matches {}", spec.files)),
            )
        }
        Err(e) => return result(Source::Run, Outcome::error("journeys", e)),
    };
    let mut checks = Vec::new();
    let mut facts = std::collections::BTreeMap::new();
    let mut runnable = Vec::new();
    let mut open_total = 0;
    for path in &paths {
        let info = match journey::inspect(path) {
            Ok(i) => i,
            Err(e) => {
                checks.push(Check::fail(path.display().to_string(), e));
                continue;
            }
        };
        open_total += info.open_loop;
        facts.insert(format!("open_loop/{}", info.id), info.open_loop.to_string());
        if let Err(e) = journey::check_pad(&info, &spec.forced_analog) {
            checks.push(Check::fail(format!("{} pad", info.id), e));
            continue;
        }
        if let Err(e) = disc_is_an_artifact(ctx, &info) {
            checks.push(Check::fail(format!("{} disc", info.id), e));
            continue;
        }
        let base = ctx
            .baseline
            .as_ref()
            .and_then(|b| b.journeys.get(&info.id).copied());
        if !bless {
            let allowed = base.map_or(0, |b| b.open_loop);
            if info.open_loop > allowed {
                checks.push(Check::fail(
                    format!("{} closed-loop", info.id),
                    format!(
                        "{} open-loop steps (fixed waits or frame-counted holds), the baseline \
                         allows {allowed}: wait on game state instead (wait_until / hold_until on \
                         a symbol); a new journey must have none",
                        info.open_loop
                    ),
                ));
                continue;
            }
        }
        runnable.push((info, base.map_or(0, |b| b.ticks)));
    }
    checks.push(Check::new(
        "open-loop steps (ratchet, target 0)",
        Status::Pass,
        open_total.to_string(),
    ));
    let models: Vec<&str> = match (ctx.tier, bless) {
        (_, true) | (Tier::Quick, false) => vec!["real"],
        (Tier::Full, false) => FULL_MODELS.to_vec(),
    };
    let mut jobs = Vec::new();
    for (info, ticks) in runnable {
        if ctx.tier == Tier::Quick && !bless && !spec.precheck.contains(&info.id) {
            continue;
        }
        for m in &models {
            jobs.push(Job {
                info: info.clone(),
                model: (*m).to_string(),
                expected_ticks: ticks,
            });
        }
    }
    // Longest first: the critical path is the longest journey, not the sum.
    jobs.sort_by(|a, b| {
        b.expected_ticks
            .cmp(&a.expected_ticks)
            .then(a.info.id.cmp(&b.info.id))
            .then(a.model.cmp(&b.model))
    });
    let conformance = format!("--conformance={}", spec.conformance.join(","));
    let render = if spec.hw { "--strict" } else { "--no-hw" };
    let receipts = ctx.work.join("gate-receipts");
    let width = ctx.config.budget.emulators;
    let outcomes = sched::run_all(&jobs, width, |job| {
        ctx.timed(&format!("journey:{}@{}", job.info.id, job.model), || {
            one(ctx, &gate, job, &[render, &conformance], &receipts)
        })
    });
    let mut hits = 0;
    let mut sorted: Vec<(&Job, (Check, Option<GateLine>))> = jobs.iter().zip(outcomes).collect();
    sorted.sort_by(|a, b| (&a.0.info.id, &a.0.model).cmp(&(&b.0.info.id, &b.0.model)));
    for (job, (check, line)) in sorted {
        if let Some(line) = &line {
            hits += usize::from(line.receipt_hit);
            if job.model == "real" {
                facts.insert(format!("ticks/{}", job.info.id), line.ticks.to_string());
            }
            if line.skips_unknown {
                checks.push(Check::skip(
                    format!("{}@{} skips", job.info.id, job.model),
                    "the gate answered from a receipt this harness never saw run, so what that \
                     run skipped is unknown here (run with fresh receipts to list it)",
                ));
            }
            if line.golden_skipped > 0 {
                checks.push(Check::skip(
                    format!("{}@{} golden", job.info.id, job.model),
                    format!(
                        "{} golden compare(s) not run on this model (the journey's golden_on)",
                        line.golden_skipped
                    ),
                ));
            }
            if line.skip > 0 {
                checks.push(Check::skip(
                    format!("{}@{} conformance", job.info.id, job.model),
                    format!(
                        "{} rule(s) reported, not enforced: {}",
                        line.skip,
                        not_enforced(&line.conformance)
                    ),
                ));
            }
        }
        checks.push(check);
    }
    let source = match hits {
        0 => Source::Run,
        n if n == jobs.len() => Source::Hit,
        _ => Source::Mixed,
    };
    result(source, Outcome { checks, facts })
}

fn disc_is_an_artifact(ctx: &Ctx<'_>, info: &Info) -> Result<(), String> {
    let Some(disc) = info.discs.first() else {
        return Err("the journey names no disc: name the cue a [[build]] writes".into());
    };
    let disc = ctx.repo.join(disc);
    let ok = ctx
        .game
        .builds
        .iter()
        .any(|b| disc.parent() == Some(b.out_dir(&ctx.repo).as_path()));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "it boots {}, which no [[build]] writes into its out: the harness only judges \
             bytes it built",
            disc.display()
        ))
    }
}

fn one(
    ctx: &Ctx<'_>,
    gate: &Path,
    job: &Job,
    flags: &[&str],
    receipts: &Path,
) -> (Check, Option<GateLine>) {
    let conformance = flags.join(" ");
    let name = format!("{}@{}", job.info.id, job.model);
    let model_dir = job.model.replace([',', '='], "-");
    let out = ctx
        .work
        .join("journeys")
        .join(&model_dir)
        .join(&job.info.id);
    // The gate refuses to overwrite card receipts of an earlier run: each run starts clean.
    if out.exists() {
        if let Err(e) = std::fs::remove_dir_all(&out) {
            return (
                Check::fail(name, format!("clear {}: {e}", out.display())),
                None,
            );
        }
    }
    let log = ctx
        .work
        .join("journeys")
        .join("logs")
        .join(format!("{}@{model_dir}.log", job.info.id));
    let argv: Vec<String> = [
        gate.display().to_string(),
        "run".into(),
        job.info.path.display().to_string(),
        flags[0].to_string(),
        "--hardware".into(),
        job.model.clone(),
        "--receipts".into(),
        receipts.display().to_string(),
        "--out".into(),
        out.display().to_string(),
        flags[1].to_string(),
    ]
    .into();
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = match util::run_logged(&argv, &ctx.repo, &env, &log, None) {
        Ok(d) => d,
        Err(e) => return (Check::fail(name, e), None),
    };
    let mut parsed = parse_gate(&done.output);
    if let Some(line) = parsed.as_mut() {
        remember_skips(ctx, gate, job, &conformance, line);
    }
    let check = match &parsed {
        Some(l) if done.code == 0 && l.passed => Check::pass(
            &name,
            format!(
                "{} ticks, {}/{} checks{}",
                l.ticks,
                l.pass,
                l.checks,
                if l.receipt_hit { ", receipt" } else { "" }
            ),
        )
        .detail(log.display().to_string()),
        Some(l) => Check::fail(
            &name,
            format!(
                "{} of {} checks failed{}{}; log {}",
                l.fail,
                l.checks,
                if l.aborts.is_empty() {
                    String::new()
                } else {
                    format!(", abort: {}", l.aborts.join("; "))
                },
                l.conformance
                    .iter()
                    .find(|c| !c.contains("[not enforced]"))
                    .map_or(String::new(), |c| format!(", {c}")),
                log.display()
            ),
        ),
        None => Check::fail(
            &name,
            format!(
                "the gate exited {} without a verdict line; log {}",
                done.code,
                log.display()
            ),
        ),
    };
    (check, parsed)
}

/// Paths of the journeys a game file selects (for `lint`).
pub fn files(repo: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    util::glob(repo, pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receipt_hit_and_a_skip_are_read() {
        let out = "RECEIPT HIT game-sfx-menus 5f54124f769b567b (passed 2026-11-16, 38.6 s saved)\n\
PASS game           85 checks: 84 pass, 0 fail, 1 skip | 1822 ticks (30 s emulated) in 27.9 s | disc 0244409c4715 | GP0(1Fh) 5168\n\
CONFORMANCE game gpu-irq-command=5168 first at 0x8007216c [not enforced] (114 CD acknowledges, 5168 GP0(1Fh))\n";
        let l = parse_gate(out).unwrap();
        assert!(l.passed && l.receipt_hit);
        assert_eq!(
            (l.checks, l.pass, l.fail, l.skip, l.ticks),
            (85, 84, 0, 1, 1822)
        );
        assert_eq!(l.conformance.len(), 1);
    }

    #[test]
    fn a_failure_and_golden_skips_are_read() {
        let out = "  ABORT  credits: step 3 timed out\nFAIL credits 7 checks: 4 pass, 3 fail, 2 golden skipped (hardware) | 536 ticks (9 s emulated) in 4.0 s | disc ab\n";
        let l = parse_gate(out).unwrap();
        assert!(!l.passed);
        assert_eq!((l.fail, l.golden_skipped, l.ticks), (3, 2, 536));
        assert_eq!(l.aborts, ["credits: step 3 timed out"]);
        assert!(parse_gate("no verdict").is_none());
    }
}
