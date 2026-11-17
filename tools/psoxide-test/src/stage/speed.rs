//! Speed: ONE metric for every game, from ONE tool. `psoxide-pgo measure` replays a poll-bound
//! tape on the PSoXide frontend and reports, over the window, frames presented (`flips`), work
//! cycles per frame (mean and p99) and frames by vblank count. Each window runs at its declared
//! phase offsets (quick: 0 only); the tolerance per metric is the spread `bless` measured over
//! those offsets (baseline::WindowBase::tolerance).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::baseline::WindowBase;
use crate::game::Window;
use crate::report::{Check, Outcome, Source, StageResult, Status};
use crate::sched;
use crate::stage::{Ctx, Tier};
use crate::util::{self, Key};

/// The metrics read from one replay.
pub const METRICS: &[&str] = &[
    "ticks",
    "flips",
    "work_per_frame",
    "frame_work_p99",
    "vblanks",
];

/// Read `m.<metric>=value` lines.
pub fn parse_measure(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("m."))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| METRICS.contains(k))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}

/// Judge one metric over the offsets that ran. `higher_is_better` for flips.
pub fn judge_metric(
    name: &str,
    now: &[(i32, u64)],
    base: &WindowBase,
    values: &[u64],
    higher_is_better: bool,
) -> Check {
    let tolerance = WindowBase::tolerance(values);
    let mut bad = 0.0;
    let mut count = 0.0;
    let mut base_sum = 0.0;
    let mut now_sum = 0.0;
    for (offset, v) in now {
        let Some(b) = base.at(values, *offset) else {
            return Check::fail(
                name,
                format!(
                    "the baseline has no offset {offset}: bless again with the declared offsets"
                ),
            );
        };
        let delta = *v as f64 - b as f64;
        bad += if higher_is_better { -delta } else { delta };
        count += 1.0;
        base_sum += b as f64;
        now_sum += *v as f64;
    }
    if count == 0.0 {
        return Check::fail(name, "no offset ran");
    }
    let mean_bad = bad / count;
    let status = if mean_bad <= tolerance + 1e-9 {
        Status::Pass
    } else {
        Status::Fail
    };
    Check::new(
        name,
        status,
        format!("{:.1} (mean of {count:.0} offset(s))", now_sum / count),
    )
    .baseline(format!("{:.1}, tolerance {tolerance:.2}", base_sum / count))
    .detail(if status == Status::Fail {
        format!("worse by {mean_bad:.2} on average, beyond the measured phase spread")
    } else {
        String::new()
    })
}

/// One offset of one window: the offset and the metrics read there.
type Row = (i32, BTreeMap<String, String>);

struct Job<'w> {
    window: &'w Window,
    offset: i32,
    disc: PathBuf,
    disc_digest: String,
}

/// The speed stage. `all_offsets` is set by `bless` and by the full tier.
pub fn run(ctx: &Ctx<'_>, all_offsets: bool) -> StageResult {
    let result = |source, outcome| StageResult {
        name: "speed".into(),
        source,
        key: String::new(),
        outcome,
    };
    if ctx.game.windows.is_empty() {
        return result(
            Source::Run,
            Outcome {
                checks: vec![Check::skip(
                    "speed",
                    "the game file has no [[speed]] window",
                )],
                ..Outcome::default()
            },
        );
    }
    let frontend = match ctx
        .config
        .tool(&ctx.config.tools.frontend, "[tools] frontend")
    {
        Ok(f) => f.to_path_buf(),
        Err(e) => return result(Source::Run, Outcome::error("frontend", e)),
    };
    let pgo = match pgo_binary(ctx) {
        Ok(p) => p,
        Err(e) => return result(Source::Run, Outcome::error("psoxide-pgo", e)),
    };
    let tools_digest = format!(
        "{} {}",
        util::sha256_file(&frontend).unwrap_or_default(),
        util::sha256_file(&pgo).unwrap_or_default()
    );
    let windows: Vec<&Window> = if all_offsets || ctx.tier == Tier::Full {
        ctx.game.windows.iter().collect()
    } else {
        ctx.game.windows.iter().take(1).collect()
    };
    let mut jobs = Vec::new();
    let mut unmeasurable = Vec::new();
    for w in windows {
        let Some(build) = ctx.game.build(&w.build) else {
            continue;
        };
        if w.polls.is_none() {
            unmeasurable.push(Check::fail(
                &w.name,
                "this window starts and ends on game-state conditions (from / until), which \
                 psoxide-pgo measure cannot do yet: it measures poll windows of a tape only \
                 (phase 2 of the harness)",
            ));
            continue;
        }
        let disc = build.cue(&ctx.repo);
        let disc_digest = ctx
            .artifacts
            .get(&w.build)
            .and_then(|a| a.get(&build.disc.replace(".cue", ".bin")))
            .cloned()
            .unwrap_or_default();
        let offsets: Vec<i32> = if all_offsets || ctx.tier == Tier::Full {
            w.offsets.clone()
        } else {
            vec![0]
        };
        for offset in offsets {
            jobs.push(Job {
                window: w,
                offset,
                disc: disc.clone(),
                disc_digest: disc_digest.clone(),
            });
        }
    }
    let width = ctx.config.budget.emulators;
    let outcomes = sched::run_all(&jobs, width, |job| {
        ctx.timed(&format!("speed:{}@{}", job.window.name, job.offset), || {
            measure_one(ctx, job, &frontend, &pgo, &tools_digest)
        })
    });
    let mut checks = unmeasurable;
    let mut facts = BTreeMap::new();
    let mut hits = 0;
    let mut by_window: BTreeMap<&str, Vec<Row>> = BTreeMap::new();
    for (job, (metrics, hit)) in jobs.iter().zip(outcomes) {
        hits += usize::from(hit);
        match metrics {
            Ok(m) => {
                for (k, v) in &m {
                    facts.insert(format!("{}/{}/{k}", job.window.name, job.offset), v.clone());
                }
                by_window
                    .entry(job.window.name.as_str())
                    .or_default()
                    .push((job.offset, m));
            }
            Err(e) => checks.push(Check::fail(
                format!("{} at offset {}", job.window.name, job.offset),
                e,
            )),
        }
    }
    for (name, rows) in &by_window {
        let base = ctx.baseline.as_ref().and_then(|b| b.speed.get(*name));
        let Some(base) = base else {
            checks.push(Check::fail(
                format!("{name} baseline"),
                "no [speed] table for this window in the baseline: run `psoxide-test bless`",
            ));
            continue;
        };
        let series = |key: &str| -> Vec<(i32, u64)> {
            rows.iter()
                .filter_map(|(o, m)| m.get(key).and_then(|v| v.parse().ok()).map(|v| (*o, v)))
                .collect()
        };
        checks.push(judge_metric(
            &format!("{name} frames presented"),
            &series("flips"),
            base,
            &base.flips,
            true,
        ));
        checks.push(judge_metric(
            &format!("{name} cycles per frame"),
            &series("work_per_frame"),
            base,
            &base.work_per_frame,
            false,
        ));
        checks.push(judge_metric(
            &format!("{name} cycles per frame p99"),
            &series("frame_work_p99"),
            base,
            &base.frame_work_p99,
            false,
        ));
        let vblanks: Vec<String> = rows
            .iter()
            .map(|(o, m)| format!("{o}:{}", m.get("vblanks").map_or("?", String::as_str)))
            .collect();
        checks.push(Check::pass(
            format!("{name} frames by vblank count"),
            vblanks.join(" "),
        ));
    }
    let source = match hits {
        0 => Source::Run,
        n if n == jobs.len() => Source::Hit,
        _ => Source::Mixed,
    };
    result(source, Outcome { checks, facts })
}

/// Build the game's hydrated psoxide-pgo (the SDK pin's own tool) once and return its path.
fn pgo_binary(ctx: &Ctx<'_>) -> Result<PathBuf, String> {
    let manifest = ctx.repo.join(".psoxide/tools/psoxide-pgo/Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "{} is missing: the game's build hydrates .psoxide; build first",
            manifest.display()
        ));
    }
    let log = ctx.dir(&["speed"])?.join("psoxide-pgo-build.log");
    let argv: Vec<String> = [
        "cargo",
        "build",
        "-q",
        "--release",
        "--locked",
        "--manifest-path",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain([manifest.display().to_string()])
    .collect();
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = util::run_logged(&argv, &ctx.repo, &env, &log, None)?;
    if done.code != 0 {
        return Err(format!(
            "building psoxide-pgo failed; log {}",
            log.display()
        ));
    }
    let bin = ctx.repo.join(".psoxide/target/release/psoxide-pgo");
    if bin.is_file() {
        Ok(bin)
    } else {
        Err(format!("{} not found after the build", bin.display()))
    }
}

fn measure_one(
    ctx: &Ctx<'_>,
    job: &Job<'_>,
    frontend: &Path,
    pgo: &Path,
    tools_digest: &str,
) -> (Result<BTreeMap<String, String>, String>, bool) {
    let w = job.window;
    let (Some([first, last]), Some(tape)) = (w.polls, w.tape.as_ref()) else {
        return (Err("a tape window needs tape and polls".into()), false);
    };
    let from = i64::from(first) + i64::from(job.offset);
    let to = i64::from(last) + i64::from(job.offset);
    let tape = ctx.repo.join(tape);
    let mut key = Key::new("speed");
    key.part("disc", &job.disc_digest);
    key.part("tools", tools_digest);
    key.part("window", &format!("{from}..{to} {:?}", w.launch_args));
    if let Err(e) = key.file("tape", &tape) {
        return (Err(e), false);
    }
    let key = key.finish();
    if let Some(o) = ctx.receipts.get("speed", &key) {
        println!(
            "RECEIPT HIT speed {}@{} {}",
            w.name,
            job.offset,
            util::short(&key)
        );
        return (Ok(o.facts), true);
    }
    let log = match ctx.dir(&["speed"]) {
        Ok(d) => d.join(format!(
            "{}@{}-{}.log",
            w.name,
            job.offset,
            util::short(&key)
        )),
        Err(e) => return (Err(e), false),
    };
    let mut argv: Vec<String> = vec![
        pgo.display().to_string(),
        "measure".into(),
        "--frontend".into(),
        frontend.display().to_string(),
        "--image".into(),
        job.disc.display().to_string(),
        "--tape".into(),
        tape.display().to_string(),
        "--polls".into(),
        format!("{from}..{to}"),
        "--name".into(),
        "m".into(),
    ];
    for a in &w.launch_args {
        argv.push("--launch-arg".into());
        argv.push(a.clone());
    }
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let metrics = match util::run_logged(&argv, &ctx.repo, &env, &log, None) {
        Ok(d) if d.code == 0 => {
            let m = parse_measure(&d.output);
            if m.contains_key("flips") && m.contains_key("work_per_frame") {
                Ok(m)
            } else {
                Err(format!("measure printed no metrics; log {}", log.display()))
            }
        }
        Ok(d) => Err(format!("measure exited {}; log {}", d.code, log.display())),
        Err(e) => Err(e),
    };
    if let Ok(m) = &metrics {
        let outcome = Outcome {
            checks: vec![Check::pass("measured", "")],
            facts: m.clone(),
        };
        let _ = ctx.receipts.put("speed", &key, &outcome);
    }
    (metrics, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> WindowBase {
        WindowBase {
            offsets: vec![-12, 0, 12],
            flips: vec![790, 786, 778],
            work_per_frame: vec![100, 100, 100],
            frame_work_p99: vec![10, 10, 10],
            vblanks: vec![String::new(); 3],
        }
    }

    #[test]
    fn one_flip_down_everywhere_is_inside_a_four_flip_spread() {
        let b = base();
        let now = [(-12, 789), (0, 785), (12, 777)];
        let c = judge_metric("f", &now, &b, &b.flips, true);
        assert_eq!(c.status, Status::Pass, "{c:?}");
        let now = [(-12, 780), (0, 776), (12, 768)];
        assert_eq!(
            judge_metric("f", &now, &b, &b.flips, true).status,
            Status::Fail
        );
    }

    #[test]
    fn zero_spread_means_no_regression_is_allowed() {
        let b = base();
        let worse = [(0, 101)];
        assert_eq!(
            judge_metric("w", &worse, &b, &b.work_per_frame, false).status,
            Status::Fail
        );
        let better = [(0, 99)];
        assert_eq!(
            judge_metric("w", &better, &b, &b.work_per_frame, false).status,
            Status::Pass
        );
    }

    #[test]
    fn measure_lines_are_read() {
        let m =
            parse_measure("m.ticks=804\nm.flips=778\nm.vblanks=1:753,2:25\nm.cycles=5\nnoise\n");
        assert_eq!(m["flips"], "778");
        assert_eq!(m["vblanks"], "1:753,2:25");
        assert!(!m.contains_key("cycles"));
    }
}
