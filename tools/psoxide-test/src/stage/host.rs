//! Host checks: commands the game declares (its own tests, format check, hazard scans, an older
//! frontend or a gameplay script run against a built disc) that must exit 0 and leave the tree as
//! they found it, optionally once per hardware model and optionally yielding a number that is
//! judged like a bar. Keyed by the command, its environment, the tree's bytes and, with `build`,
//! that build's artifacts. They run one at a time, so one that starts an emulator stays inside
//! the budget.

use std::path::PathBuf;

use crate::game::{Host, Metric};
use crate::report::{Check, Outcome, Source, StageResult, Status};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// The first integer after `key=` or `key:` in `output`.
pub fn read_metric(output: &str, key: &str) -> Option<u64> {
    for sep in ["=", ":"] {
        let needle = format!("{key}{sep}");
        if let Some(at) = output.find(&needle) {
            let rest = output[at + needle.len()..].trim_start();
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '_')
                .filter(char::is_ascii_digit)
                .collect();
            if let Ok(v) = digits.parse() {
                return Some(v);
            }
        }
    }
    None
}

/// Judge a metric value: the absolute bars always, the baseline unless blessing.
pub fn judge_metric(name: &str, m: &Metric, value: u64, base: Option<u64>, bless: bool) -> Check {
    let mut problems = Vec::new();
    if let Some(min) = m.min.filter(|min| value < *min) {
        problems.push(format!("under the floor {min}"));
    }
    if let Some(max) = m.max.filter(|max| value > *max) {
        problems.push(format!("over the ceiling {max}"));
    }
    let mut check = Check::new(format!("{name} {}", m.key), Status::Pass, value.to_string());
    if let (Some(better), false) = (m.better.as_deref(), bless) {
        match base {
            Some(b) => {
                check = check.baseline(format!("{b}, tolerance {}", m.tolerance));
                let worse = if better == "lower" {
                    value.saturating_sub(b)
                } else {
                    b.saturating_sub(value)
                };
                if worse > m.tolerance {
                    problems.push(format!("{worse} worse than the baseline"));
                }
            }
            None => problems.push("no baseline value: run `psoxide-test bless`".into()),
        }
    }
    if problems.is_empty() {
        check
    } else {
        check.status = Status::Fail;
        check.detail(problems.join("; "))
    }
}

/// The host stage. `bless` records metrics instead of judging them against the baseline.
/// The variable a game's host check reads for the landing allowance, in permille.
pub const LANDING_SLACK_ENV: &str = "PSOXIDE_LANDING_SLACK_PERMILLE";
/// The landing allowance on a game's own host ceilings: 5%, as on the speed rows.
pub const LANDING_HOST_SLACK_PERMILLE: u32 = 50;

pub fn run(ctx: &Ctx<'_>, bless: bool) -> StageResult {
    let mut checks = Vec::new();
    let mut facts = std::collections::BTreeMap::new();
    let mut runs = 0;
    let mut hits = 0;
    let files = util::tree_files(&ctx.repo).unwrap_or_default();
    let mut tree = Key::new("tree");
    for f in &files {
        if let Err(e) = tree.file(f, &ctx.repo.join(f)) {
            checks.push(Check::fail("tree", e));
        }
    }
    let tree = tree.finish();
    // Jobs in declared order, grouped: a host's `exclusive` group runs one job after another,
    // every other job is a group of its own; groups run in parallel inside the CPU budget.
    let mut groups: Vec<(Option<&str>, Vec<HostJob<'_>>)> = Vec::new();
    for host in &ctx.game.hosts {
        let models: Vec<Option<&str>> = if host.models.is_empty() {
            vec![None]
        } else {
            host.models.iter().map(|m| Some(m.as_str())).collect()
        };
        for model in models {
            runs += 1;
            let name = match model {
                Some(m) => format!("{}@{m}", host.name),
                None => host.name.clone(),
            };
            let group = host.exclusive.as_deref();
            match groups
                .iter_mut()
                .find(|(g, _)| group.is_some() && *g == group)
            {
                Some((_, jobs)) => jobs.push((host, model, name)),
                None => groups.push((group, vec![(host, model, name)])),
            }
        }
    }
    let before = util::git(&ctx.repo, &["status", "--porcelain"]).unwrap_or_default();
    let done = crate::sched::run_all(&groups, ctx.jobs_width(), |(_, jobs)| {
        jobs.iter()
            .map(|(host, model, name)| {
                (
                    name.clone(),
                    one(ctx, host, *model, name, &tree, &files, bless),
                )
            })
            .collect::<Vec<_>>()
    });
    let mut results: Vec<HostRow> = done.into_iter().flatten().collect();
    // Report in the order the game file declares.
    let order: Vec<String> = groups
        .iter()
        .flat_map(|(_, jobs)| jobs.iter().map(|(_, _, n)| n.clone()))
        .collect();
    results.sort_by_key(|(n, _)| order.iter().position(|o| o == n));
    for (name, r) in results {
        match r {
            Ok((outcome, hit)) => {
                hits += usize::from(hit);
                facts.extend(outcome.facts);
                checks.extend(outcome.checks);
            }
            Err(e) => checks.push(Check::fail(&name, e)),
        }
    }
    // Checks run side by side, so the tree is compared once around all of them.
    let after = util::git(&ctx.repo, &["status", "--porcelain"]).unwrap_or_default();
    if before != after {
        checks.push(Check::fail(
            "host checks left the tree as they found it",
            format!(
                "the tree changed (stale committed files?): {}",
                changed(&before, &after)
            ),
        ));
    }
    if ctx.game.hosts.is_empty() {
        checks.push(Check::skip(
            "host",
            "the game file declares no [[host]] checks",
        ));
    }
    StageResult {
        name: "host".into(),
        source: if hits == 0 {
            Source::Run
        } else if hits == runs {
            Source::Hit
        } else {
            Source::Mixed
        },
        key: String::new(),
        outcome: Outcome { checks, facts },
    }
}

/// The digest of what a `uses` marker names.
fn uses_digest(ctx: &Ctx<'_>, marker: &str) -> Result<String, String> {
    match marker {
        "sdk" => {
            // The guest sources of the hydrated SDK by content (stat-cached), not its pin: a
            // host-tool-only SDK commit leaves them unchanged.
            let mut key = Key::new("uses-sdk");
            for part in ["sdk", "crates", "engine"] {
                let dir = ctx.repo.join(".psoxide").join(part);
                let digest = if dir.is_dir() {
                    ctx.originals
                        .lock()
                        .map_err(|_| "digest cache lock".to_string())?
                        .digest(&dir)?
                } else {
                    "missing".into()
                };
                key.part(part, &digest);
            }
            Ok(key.finish())
        }
        "gate" => Ok(crate::config::GATE_REVISION.to_string()),
        "frontend" => {
            let f = ctx
                .config
                .tool(&ctx.config.tools.frontend, "[tools] frontend")?;
            util::sha256_file(f)
        }
        other => Err(format!("unknown uses marker `{other}`")),
    }
}

fn one(
    ctx: &Ctx<'_>,
    host: &Host,
    model: Option<&str>,
    name: &str,
    tree: &str,
    files: &[String],
    bless: bool,
) -> Result<(Outcome, bool), String> {
    let mut command = Vec::new();
    let mut keyed = Vec::new();
    for arg in &host.command {
        command.push(ctx.resolve(arg)?);
        keyed.push(ctx.key_text(arg)?);
    }
    let mut key = Key::new("host");
    // Original game files by content, not path (originals.rs).
    key.part("command", &keyed.join("\0"));
    key.part("dir", host.dir.as_deref().unwrap_or("."));
    key.part("model", model.unwrap_or(""));
    if host.inputs.is_empty() {
        key.part("tree", tree);
    } else {
        // Only what the check declares it reads.
        for f in files
            .iter()
            .filter(|f| host.inputs.iter().any(|i| f.starts_with(i.as_str())))
        {
            key.file(f, &ctx.repo.join(f))?;
        }
        for u in &host.uses {
            key.part(&format!("uses {u}"), &uses_digest(ctx, u)?);
        }
    }
    let mut env = ctx.command_env(&ctx.work.join("guest-stage"), &host.env, &host.env_remove)?;
    if let Some(m) = model {
        env.push(("PSOXIDE_HARDWARE".into(), Some(m.to_string())));
    }
    // Under the landing profile a game's own layout-sensitive host ceilings (frame pacing) get the
    // same allowance as the speed rows, read by the game from this variable; the committed
    // ceilings stay the install bar. It is part of the receipt key, so a landing pass never
    // answers for an install run.
    if ctx.profile == super::Profile::Landing {
        env.push((
            LANDING_SLACK_ENV.into(),
            Some(LANDING_HOST_SLACK_PERMILLE.to_string()),
        ));
    }
    for (k, v) in &env {
        match host.env.get(k) {
            Some(declared) if v.is_some() => key.part(k, &ctx.key_text(declared)?),
            _ => key.part(k, v.as_deref().unwrap_or("(removed)")),
        }
    }
    if let Some(build) = &host.build {
        let b = ctx
            .game
            .build(build)
            .ok_or_else(|| format!("unknown build `{build}`"))?;
        let out = b.out_dir(&ctx.repo).display().to_string();
        let cue = b.cue(&ctx.repo).display().to_string();
        for arg in &mut command {
            *arg = arg.replace("{out}", &out).replace("{disc}", &cue);
        }
        // The build's bytes, not its link map: a map names object paths that move with the SDK
        // checkout while the code stays the same (the symbol list stands for it).
        for (file, digest) in ctx
            .artifacts
            .get(build)
            .into_iter()
            .flatten()
            .filter(|(f, _)| !f.ends_with(".map"))
        {
            key.part(file, digest);
        }
    }
    let key = key.finish();
    let base = ctx
        .baseline
        .as_ref()
        .and_then(|b| b.host.get(name).copied());
    // The stored outcome holds the measured value; it is judged again against today's baseline.
    let (outcome, hit) = match ctx.receipts.get("host", &key) {
        Some(o) => {
            println!("RECEIPT HIT host {name} {}", util::short(&key));
            (o, true)
        }
        None => {
            let o = ctx.timed(&format!("host:{name}"), || {
                execute(ctx, host, name, &command, &env, &key)
            });
            let _ = ctx.receipts.put("host", &key, &o);
            (o, false)
        }
    };
    let mut checks = outcome.checks.clone();
    if let (Some(m), Some(v)) = (&host.metric, outcome.facts.get(&format!("metric/{name}"))) {
        let value: u64 = v.parse().unwrap_or(0);
        checks.push(judge_metric(name, m, value, base, bless));
    }
    Ok((
        Outcome {
            checks,
            facts: outcome.facts,
        },
        hit,
    ))
}

fn execute(
    ctx: &Ctx<'_>,
    host: &Host,
    name: &str,
    command: &[String],
    env: &[(String, Option<String>)],
    key: &str,
) -> Outcome {
    let log = match ctx.dir(&["host"]) {
        Ok(d) => d.join(format!(
            "{}-{}.log",
            name.replace([',', '='], "-"),
            util::short(key)
        )),
        Err(e) => return Outcome::error(name, e),
    };
    let dir: PathBuf = host
        .dir
        .as_ref()
        .map_or_else(|| ctx.repo.clone(), |d| ctx.repo.join(d));
    let run = || util::run_logged(command, &dir, env, &log, None);
    let done = match host.emulators {
        Some(n) if n > 0 => {
            let b = &ctx.config.budget;
            crate::sched::machine_slots(b.shared_dir.as_deref(), "emu", b.machine_emulators, n, run)
        }
        _ => run(),
    };
    let mut facts = std::collections::BTreeMap::new();
    let check = match done {
        Ok(d) if d.code != 0 => {
            Check::fail(name, format!("exit {}; log {}", d.code, log.display()))
        }
        Ok(d) => match &host.metric {
            Some(m) => match read_metric(&d.output, &m.key) {
                Some(v) => {
                    facts.insert(format!("metric/{name}"), v.to_string());
                    Check::pass(name, "exit 0")
                }
                None => Check::fail(
                    name,
                    format!(
                        "exit 0 but no `{}=` or `{}:` value in log {}",
                        m.key,
                        m.key,
                        log.display()
                    ),
                ),
            },
            None => Check::pass(name, "exit 0"),
        },
        Err(e) => Check::fail(name, e),
    };
    Outcome {
        checks: vec![check],
        facts,
    }
}

/// One host job: the check, the model it runs on (if any) and its row name.
type HostJob<'a> = (&'a Host, Option<&'a str>, String);
/// A row name and what its job returned.
type HostRow = (String, Result<(Outcome, bool), String>);

fn changed(before: &str, after: &str) -> String {
    after
        .lines()
        .filter(|l| !before.lines().any(|b| b == *l))
        .map(|l| l.trim().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(better: Option<&str>, tolerance: u64, min: Option<u64>) -> Metric {
        Metric {
            key: "heap_free".into(),
            better: better.map(str::to_string),
            tolerance,
            reason: None,
            min,
            max: None,
        }
    }

    #[test]
    fn a_metric_is_read_after_key_equals_or_colon() {
        assert_eq!(
            read_metric("x\nheap_free=12,288 B\n", "heap_free"),
            Some(12288)
        );
        assert_eq!(read_metric("cycles: 5_000\n", "cycles"), Some(5000));
        assert_eq!(read_metric("nothing here", "cycles"), None);
    }

    #[test]
    fn floors_and_the_baseline_are_judged() {
        let m = metric(Some("higher"), 0, Some(8192));
        assert_eq!(
            judge_metric("boot", &m, 8000, Some(8000), false).status,
            Status::Fail
        );
        assert_eq!(
            judge_metric("boot", &m, 9000, Some(9000), false).status,
            Status::Pass
        );
        assert_eq!(
            judge_metric("boot", &m, 8999, Some(9000), false).status,
            Status::Fail
        );
        assert_eq!(
            judge_metric("boot", &m, 8999, None, true).status,
            Status::Pass
        );
        let lower = metric(Some("lower"), 100, None);
        assert_eq!(
            judge_metric("bench", &lower, 1100, Some(1000), false).status,
            Status::Pass
        );
        assert_eq!(
            judge_metric("bench", &lower, 1101, Some(1000), false).status,
            Status::Fail
        );
    }
}
