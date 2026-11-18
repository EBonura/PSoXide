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
    /// The window in polls: declared, or located from its conditions.
    polls: [u32; 2],
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
    let mut located_facts = BTreeMap::new();
    for w in windows {
        let Some(build) = ctx.game.build(&w.build) else {
            continue;
        };
        let disc = build.cue(&ctx.repo);
        let disc_digest = ctx
            .artifacts
            .get(&w.build)
            .and_then(|a| a.get(&build.disc.replace(".cue", ".bin")))
            .cloned()
            .unwrap_or_default();
        let polls = match w.polls {
            Some(p) => p,
            None => match locate(ctx, w, build, &disc, &disc_digest, &frontend) {
                Ok(p) => {
                    located_facts
                        .insert(format!("{}/polls", w.name), format!("{}..{}", p[0], p[1]));
                    p
                }
                Err(e) => {
                    unmeasurable.push(Check::fail(format!("{} window", w.name), e));
                    continue;
                }
            },
        };
        let offsets: Vec<i32> = if all_offsets || ctx.tier == Tier::Full {
            w.offsets.clone()
        } else {
            vec![0]
        };
        for offset in offsets {
            jobs.push(Job {
                window: w,
                polls,
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
    let mut facts = located_facts;
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

/// Build the psoxide-pgo of the game's SDK pin once and return its path: from the game's own
/// hydrated `.psoxide` (a real directory, never a symlink to a checkout, which a clean clone
/// would not have), or else from the user config's `[tools] sdk` checkout, which must be at the
/// game's pinned SDK revision. The second form builds into `<work>/sdk-tools/<revision>` and
/// never writes into the checkout.
fn pgo_binary(ctx: &Ctx<'_>) -> Result<PathBuf, String> {
    let hydrated = ctx.repo.join(".psoxide");
    let linked = std::fs::symlink_metadata(&hydrated).is_ok_and(|m| m.file_type().is_symlink());
    let own = hydrated.join("tools/psoxide-pgo/Cargo.toml");
    let (manifest, target) = if own.is_file() && !linked {
        (own, None)
    } else if let Some(sdk) = &ctx.config.tools.sdk {
        let manifest = sdk.join("tools/psoxide-pgo/Cargo.toml");
        if !manifest.is_file() {
            return Err(format!(
                "[tools] sdk {} has no tools/psoxide-pgo: name a full SDK checkout",
                sdk.display()
            ));
        }
        let head = util::git(sdk, &["rev-parse", "HEAD"])?;
        let pin = super::sdk_pin(&ctx.repo);
        if pin != "unknown" && pin != head {
            return Err(format!(
                "[tools] sdk {} is at {} but the game pins the SDK at {}: check that revision \
                 out there (or in a worktree the config names)",
                sdk.display(),
                util::short(&head),
                util::short(&pin)
            ));
        }
        let target = ctx.dir(&["sdk-tools", util::short(&head)])?;
        (manifest, Some(target))
    } else if linked {
        return Err(format!(
            "{} is a symlink (machine-local; a clean clone has none): remove it and set [tools] \
             sdk in the config to a full SDK checkout at the game's pin",
            hydrated.display()
        ));
    } else {
        return Err(format!(
            "{} is missing: the game's build hydrates .psoxide, or set [tools] sdk in the config \
             to a full SDK checkout at the game's pin",
            own.display()
        ));
    };
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
    let mut env = ctx.child_env(&ctx.work.join("guest-stage"));
    if let Some(t) = &target {
        env.push(("CARGO_TARGET_DIR".into(), Some(t.display().to_string())));
    }
    let done = util::run_logged(&argv, &ctx.repo, &env, &log, None)?;
    if done.code != 0 {
        return Err(format!(
            "building psoxide-pgo failed; log {}",
            log.display()
        ));
    }
    let bin = match &target {
        Some(t) => t.join("release/psoxide-pgo"),
        None => hydrated.join("target/release/psoxide-pgo"),
    };
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
    let [first, last] = job.polls;
    let from = i64::from(first) + i64::from(job.offset);
    let to = i64::from(last) + i64::from(job.offset);
    if from < 0 {
        return (
            Err(format!(
                "offset {} moves the window before poll 0",
                job.offset
            )),
            false,
        );
    }
    let tape = w.tape.as_ref().map(|t| ctx.repo.join(t));
    let mut key = Key::new("speed");
    key.part("disc", &job.disc_digest);
    key.part("tools", tools_digest);
    key.part("window", &format!("{from}..{to} {:?}", w.launch_args));
    if let Some(tape) = &tape {
        if let Err(e) = key.file("tape", tape) {
            return (Err(e), false);
        }
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
    ];
    if let Some(tape) = &tape {
        argv.push("--tape".into());
        argv.push(tape.display().to_string());
    }
    argv.extend([
        "--polls".into(),
        format!("{from}..{to}"),
        "--name".into(),
        "m".into(),
    ]);
    for a in &w.launch_args {
        argv.push("--launch-arg".into());
        argv.push(a.clone());
    }
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let metrics = match ctx.machine("emu", || {
        util::run_logged(&argv, &ctx.repo, &env, &log, None)
    }) {
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

/// Steps cap of the locating replay (the same ceiling psoxide-pgo gives its replays).
const LOCATE_STEPS: &str = "40000000000";

/// Find the polls of a window declared by conditions: one plain replay of the window's tape
/// (no profiling logs) with the condition words on the route log. The window starts after the
/// poll in whose route tick `from` first holds and ends at the poll in whose tick `until` first
/// holds after that. Receipt-keyed per window, so every offset shares one locating replay.
fn locate(
    ctx: &Ctx<'_>,
    w: &Window,
    build: &crate::game::Build,
    disc: &Path,
    disc_digest: &str,
    frontend: &Path,
) -> Result<[u32; 2], String> {
    let (Some(from), Some(until), Some(max_polls)) = (&w.from, &w.until, w.max_polls) else {
        return Err("a window by conditions needs from, until and max_polls".into());
    };
    let map = build
        .map_path(&ctx.repo)
        .ok_or_else(|| format!("build `{}` declares no map", build.name))?;
    let mut symbols = crate::journey::Symbols::default();
    symbols.load(&map)?;
    let from = crate::journey::condition(from, &symbols)?;
    let until = crate::journey::condition(until, &symbols)?;
    let tape = w.tape.as_ref().map(|t| ctx.repo.join(t));
    let mut key = Key::new("speed-locate");
    key.part("disc", disc_digest);
    key.file("frontend", frontend)?;
    if let Some(tape) = &tape {
        key.file("tape", tape)?;
    }
    for probe in [&from, &until] {
        key.part(
            "probe",
            &serde_json::to_string(probe).map_err(|e| e.to_string())?,
        );
    }
    key.part("limit", &format!("{max_polls} {:?}", w.launch_args));
    let key = key.finish();
    let read = |o: &Outcome| -> Option<[u32; 2]> {
        Some([
            o.facts.get("from")?.parse().ok()?,
            o.facts.get("until")?.parse().ok()?,
        ])
    };
    if let Some(found) = ctx
        .receipts
        .get("speed-locate", &key)
        .as_ref()
        .and_then(read)
    {
        println!("RECEIPT HIT speed-locate {} {}", w.name, util::short(&key));
        return Ok(found);
    }
    let dir = ctx.dir(&["speed", "locate"])?;
    let stem = format!("{}-{}", w.name, util::short(&key));
    let route = dir.join(format!("{stem}.route.csv"));
    let log = dir.join(format!("{stem}.log"));
    let mut argv: Vec<String> = vec![
        frontend.display().to_string(),
        "launch".into(),
        "--path".into(),
        disc.display().to_string(),
    ];
    if let Some(tape) = &tape {
        argv.push("--input-tape".into());
        argv.push(tape.display().to_string());
    }
    argv.extend([
        "--stop-at-poll".into(),
        max_polls.to_string(),
        "--steps".into(),
        LOCATE_STEPS.into(),
        "--route-log".into(),
        route.display().to_string(),
    ]);
    let mut words: Vec<u32> = [from.addr & !3, until.addr & !3].to_vec();
    words.dedup();
    for word in &words {
        argv.push("--route-watch-u32".into());
        argv.push(format!("{word:#010x}"));
    }
    argv.extend(w.launch_args.iter().cloned());
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = ctx.machine("emu", || {
        util::run_logged(&argv, &ctx.repo, &env, &log, None)
    })?;
    if done.code != 0 {
        return Err(format!(
            "the locating replay exited {}; log {}",
            done.code,
            log.display()
        ));
    }
    let text =
        std::fs::read_to_string(&route).map_err(|e| format!("read {}: {e}", route.display()))?;
    let found = find_window(&text, &from, &until, max_polls)?;
    let outcome = Outcome {
        checks: vec![Check::pass("located", "")],
        facts: BTreeMap::from([
            ("from".to_string(), found[0].to_string()),
            ("until".to_string(), found[1].to_string()),
        ]),
    };
    let _ = ctx.receipts.put("speed-locate", &key, &outcome);
    println!(
        "speed window {} located at polls {}..{} ({} .. {})",
        w.name, found[0], found[1], from.text, until.text
    );
    Ok(found)
}

/// Read a route log with `ram_XXXXXXXX` columns and return `[from poll, until poll]`.
fn find_window(
    route: &str,
    from: &crate::journey::Probe,
    until: &crate::journey::Probe,
    max_polls: u32,
) -> Result<[u32; 2], String> {
    let mut lines = route.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| format!("the route log has no {name} column"))
    };
    let polls_at = column("port1_polls")?;
    let word = |probe: &crate::journey::Probe| -> Result<(usize, u32), String> {
        let at = column(&format!("ram_{:08x}", probe.addr & !3))?;
        Ok((at, (probe.addr & 3) * 8))
    };
    let (from_at, from_shift) = word(from)?;
    let (until_at, until_shift) = word(until)?;
    let mut start: Option<u32> = None;
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let number = |at: usize| -> Option<u64> { fields.get(at)?.parse().ok() };
        let (Some(polls), Some(f), Some(u)) = (number(polls_at), number(from_at), number(until_at))
        else {
            continue;
        };
        let polls = u32::try_from(polls).map_err(|_| "poll count out of range")?;
        match start {
            None if from.holds((f as u32) >> from_shift) => start = Some(polls),
            Some(s) if polls > s && until.holds((u as u32) >> until_shift) => {
                return Ok([s, polls]);
            }
            _ => {}
        }
    }
    Err(match start {
        None => format!("`{}` never held before poll {max_polls}", from.text),
        Some(s) => format!(
            "`{}` held at poll {s} but `{}` never held after it before poll {max_polls}",
            from.text, until.text
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(addr: u32, size: u8, op: &str, value: i64) -> crate::journey::Probe {
        crate::journey::Probe {
            text: format!("{addr:x} {op} {value}"),
            addr,
            size,
            signed: false,
            op: op.into(),
            value,
            value_hi: 0,
        }
    }

    #[test]
    fn a_window_is_found_from_the_condition_words() {
        let route = "route_tick,port1_polls,ram_80001000\n\
                     0,0,0\n1,1,0\n2,2,5\n3,2,6\n4,3,9\n5,4,12\n";
        let from = probe(0x8000_1000, 4, "ge", 5);
        let until = probe(0x8000_1000, 4, "ge", 9);
        assert_eq!(find_window(route, &from, &until, 10).unwrap(), [2, 3]);
        // A byte inside the word: bits 8..15.
        let route = "route_tick,port1_polls,ram_80001000\n0,0,0\n1,1,256\n2,2,512\n";
        let from = probe(0x8000_1001, 1, "eq", 1);
        let until = probe(0x8000_1001, 1, "eq", 2);
        assert_eq!(find_window(route, &from, &until, 10).unwrap(), [1, 2]);
        let never = probe(0x8000_1001, 1, "eq", 7);
        assert!(find_window(route, &from, &never, 10)
            .unwrap_err()
            .contains("never held after"));
    }

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
