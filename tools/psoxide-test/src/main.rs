//! `psoxide-test`: one test harness for every PSoXide game.
//!
//! A game declares what to build and check in `psoxide-test.toml` at its repo root; this tool
//! owns how: stage order, content receipts, the emulator budget, pass and fail against the
//! committed baseline, one result file per run, the clean-clone proof and the install. See
//! README.md beside this crate.

mod baseline;
mod config;
mod game;
mod journey;
mod originals;
mod procs;
mod rearmed;
mod receipt;
mod redux;
mod report;
mod sched;
mod stage;
mod util;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use baseline::{Baseline, JourneyBase, WindowBase};
use config::{Config, GATE_REVISION};
use game::Game;
use receipt::Store;
use report::{Check, Outcome, RunResult, Source, StageResult, Status};
use stage::{Ctx, Tier};

const USAGE: &str = "\
usage:
  psoxide-test run [--repo DIR] [--tier quick|full] [--stage NAME]...
      the pipeline (stages: build size host journeys speed cross clean); exit 0 only when every
      check that ran passed, and for --tier full only when the landing verdict is complete
  psoxide-test bless [--repo DIR]
      measure the reference build and write psoxide-test.baseline.toml (refuses a red journey)
  psoxide-test lint [--repo DIR]
      load the game file and every journey; print the pad and open-loop findings
  psoxide-test install [--repo DIR]
      install the judged disc into the configured library (needs a complete full PASS)
config: ~/.config/psoxide-test/config.toml or PSOXIDE_TEST_CONFIG (see config.example.toml)";

const STAGES: &[&str] = &[
    "build", "size", "host", "journeys", "speed", "cross", "clean",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("psoxide-test: {e}");
            ExitCode::from(2)
        }
    }
}

struct Options {
    repo: PathBuf,
    tier: Tier,
    stages: Vec<String>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        repo: std::env::current_dir().map_err(|e| e.to_string())?,
        tier: Tier::Quick,
        stages: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--repo" => o.repo = PathBuf::from(value()?),
            "--tier" => {
                o.tier = match value()?.as_str() {
                    "quick" => Tier::Quick,
                    "full" => Tier::Full,
                    t => return Err(format!("unknown tier `{t}` (quick or full)")),
                }
            }
            "--stage" => {
                let s = value()?;
                if !STAGES.contains(&s.as_str()) {
                    return Err(format!(
                        "unknown stage `{s}` (one of {})",
                        STAGES.join(", ")
                    ));
                }
                o.stages.push(s);
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    o.repo = o
        .repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", o.repo.display()))?;
    Ok(o)
}

fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let command = args.first().map(String::as_str);
    if command == Some("reap") {
        procs::reap();
    }
    if matches!(command, Some("run" | "bless" | "install" | "adapter")) {
        // Nothing this process starts may outlive it (procs.rs).
        procs::install_signal_handlers();
        if command != Some("adapter") {
            procs::start_reaper();
        }
    }
    match command {
        Some("run") => {
            let o = options(&args[1..])?;
            run(&o)
        }
        Some("bless") => bless(&options(&args[1..])?),
        Some("lint") => lint(&options(&args[1..])?),
        Some("install") => install(&options(&args[1..])?),
        Some("adapter") => adapter(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

/// The `adapter` subcommands the harness runs in their own process.
fn adapter(args: &[String]) -> Result<ExitCode, String> {
    match args {
        [kind, plan, core, out] if kind == "rearmed" => {
            let text = std::fs::read_to_string(plan).map_err(|e| format!("read {plan}: {e}"))?;
            let plan: journey::Plan = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let r = rearmed::replay(&plan, Path::new(core), Path::new(out))?;
            println!(
                "rearmed: {} frames, {} distinct, reached {:?}, failure {:?}",
                r.frames, r.distinct, r.reached, r.failure
            );
            // Exit without unloading the core: the result is on disk.
            std::process::exit(0);
        }
        [kind, plan, contents, cfg, out] if kind == "redux" => {
            let text = std::fs::read_to_string(plan).map_err(|e| format!("read {plan}: {e}"))?;
            let plan: journey::Plan = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let seconds = (plan.max_frames / 20).clamp(120, 1800);
            let r = redux::replay(
                &plan,
                Path::new(contents),
                Path::new(cfg),
                Path::new(out),
                seconds,
            )?;
            println!(
                "redux: {} frames, {} distinct, reached {:?}, failure {:?}",
                r.frames, r.distinct, r.reached, r.failure
            );
            Ok(ExitCode::SUCCESS)
        }
        [kind, repo, smoke, pad] if kind == "plan" => {
            let repo = Path::new(repo);
            let plan = journey::plan(repo, &repo.join(smoke), pad, 60)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(
            "usage: psoxide-test adapter rearmed PLAN.json CORE.dylib OUT_DIR\n       \
                  psoxide-test adapter redux PLAN.json CONTENTS_DIR CFG_DIR OUT_DIR\n       \
                  psoxide-test adapter plan REPO SMOKE.toml PAD   (print the resolved plan)"
                .into(),
        ),
    }
}

fn context<'a>(o: &Options, game: &'a Game, config: &'a Config) -> Result<Ctx<'a>, String> {
    let safe: String = game
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let work = config.work.join(safe);
    std::fs::create_dir_all(&work).map_err(|e| format!("create {}: {e}", work.display()))?;
    Ok(Ctx {
        repo: o.repo.clone(),
        game,
        baseline: Baseline::load(&o.repo)?,
        config,
        receipts: Store::new(work.join("receipts")),
        work,
        tier: o.tier,
        artifacts: BTreeMap::new(),
        timing: Mutex::new(Vec::new()),
        originals: Mutex::new(originals::Cache::load(
            &config.work.join("input-digests.json"),
        )),
    })
}

fn not_run(name: &str, needs: &str) -> StageResult {
    StageResult {
        name: name.into(),
        source: Source::NotRun,
        key: String::new(),
        outcome: Outcome {
            checks: vec![Check::fail(
                name,
                format!("not run: needs {needs}, which failed"),
            )],
            ..Outcome::default()
        },
    }
}

fn run(o: &Options) -> Result<ExitCode, String> {
    let config = Config::load()?;
    let game = Game::load(&o.repo)?;
    let mut ctx = context(o, &game, &config)?;
    let wanted = |s: &str| {
        let in_tier = ctx_tier_has(o.tier, s);
        in_tier && (o.stages.is_empty() || o.stages.iter().any(|x| x == s))
    };
    let mut stages = Vec::new();
    let t0 = util::now_seconds();
    let build = stage::build::run(&mut ctx);
    ctx.timing
        .lock()
        .map_err(|_| "timing lock")?
        .push(report::Timing {
            what: "build".into(),
            start: t0,
            end: util::now_seconds(),
        });
    let built = build.passed();
    stages.push(build);
    for name in &STAGES[1..] {
        if !wanted(name) {
            continue;
        }
        if !built {
            stages.push(not_run(name, "build"));
            continue;
        }
        let r = ctx.timed(name, || match *name {
            "size" => stage::size::run(&ctx),
            "host" => stage::host::run(&ctx, false),
            "journeys" => stage::journeys::run(&ctx, false),
            "speed" => stage::speed::run(&ctx, false),
            "cross" => stage::cross::run(&ctx),
            "clean" => stage::clean::run(&ctx),
            _ => unreachable!("STAGES lists only these"),
        });
        stages.push(r);
    }
    let mut missing = Vec::new();
    if o.tier != Tier::Full {
        missing.push("tier full".to_string());
    }
    for s in STAGES {
        if !stages.iter().any(|r| r.name == *s) {
            missing.push(format!("stage {s}"));
        }
    }
    for (section, declared) in [
        ("[[size]]", !game.sizes.is_empty()),
        (
            "[[speed]] (or a [[host]] metric)",
            !game.windows.is_empty() || game.hosts.iter().any(|h| h.metric.is_some()),
        ),
        ("[journeys]", game.journeys.is_some()),
        ("[cross]", game.cross.is_some()),
        ("[clean]", game.clean.is_some()),
    ] {
        if !declared {
            missing.push(format!("{section} not declared"));
        }
    }
    missing.dedup();
    let commit = util::git(&o.repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let dirty = !util::git(&o.repo, &["status", "--porcelain", "--untracked-files=no"])
        .unwrap_or_default()
        .is_empty();
    let mut result = RunResult {
        schema: 1,
        game: game.name.clone(),
        commit,
        dirty,
        sdk_pin: stage::sdk_pin(&o.repo),
        harness: env!("CARGO_PKG_VERSION").into(),
        gate_revision: GATE_REVISION.into(),
        tier: o.tier.name().into(),
        stages,
        skips: Vec::new(),
        verdict: Status::Pass,
        landing_missing: missing,
    };
    result.finish();
    let timing = ctx.timing.lock().map(|t| t.clone()).unwrap_or_default();
    let path = result.write(&ctx.work.join("results"), &timing)?;
    print!("{}", result.table(&timing));
    println!("result: {}", path.display());
    let ok = result.verdict == Status::Pass
        && (o.tier == Tier::Quick || result.landing_missing.is_empty());
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn ctx_tier_has(tier: Tier, stage: &str) -> bool {
    match tier {
        Tier::Full => true,
        Tier::Quick => matches!(stage, "build" | "size" | "journeys" | "speed" | "cross"),
    }
}

fn bless(o: &Options) -> Result<ExitCode, String> {
    let config = Config::load()?;
    let game = Game::load(&o.repo)?;
    let mut ctx = context(o, &game, &config)?;
    let build = stage::build::run(&mut ctx);
    if !build.passed() {
        print_stage(&build);
        return Err("the build failed: nothing to bless".into());
    }
    let commit = util::git(&o.repo, &["rev-parse", "HEAD"])?;
    let dirty = !util::git(&o.repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    let mut base = Baseline {
        schema: 1,
        blessed_commit: if dirty {
            format!("{commit}+dirty")
        } else {
            commit
        },
        sdk_pin: stage::sdk_pin(&o.repo),
        ..Baseline::default()
    };
    for size in &game.sizes {
        let map = game
            .build(&size.build)
            .and_then(|b| b.map_path(&o.repo))
            .ok_or("[size] build has no map")?;
        let text =
            std::fs::read_to_string(&map).map_err(|e| format!("read {}: {e}", map.display()))?;
        base.size
            .insert(size.build.clone(), stage::size::measure(&text)?);
    }
    let journeys = stage::journeys::run(&ctx, true);
    print_stage(&journeys);
    if !journeys.passed() {
        return Err("a journey failed on `real`: a red is never blessed".into());
    }
    for (k, v) in &journeys.outcome.facts {
        if let Some(id) = k.strip_prefix("ticks/") {
            base.journeys.entry(id.to_string()).or_default().ticks = v.parse().unwrap_or(0);
        } else if let Some(id) = k.strip_prefix("open_loop/") {
            let e: &mut JourneyBase = base.journeys.entry(id.to_string()).or_default();
            e.open_loop = v.parse().unwrap_or(0);
        }
    }
    // The ratchet only turns one way: bless never raises a journey's open-loop count, and a
    // journey new to an existing baseline must be closed-loop.
    if let Some(old) = &ctx.baseline {
        let mut raised = Vec::new();
        for (id, now) in &base.journeys {
            let allowed = old.journeys.get(id).map_or(0, |j| j.open_loop);
            if now.open_loop > allowed {
                raised.push(format!("{id} {allowed} -> {}", now.open_loop));
            }
        }
        if !raised.is_empty() {
            return Err(format!(
                "bless lowers open-loop counts, never raises them: {}",
                raised.join(", ")
            ));
        }
    }
    let total: u32 = base.journeys.values().map(|j| j.open_loop).sum();
    println!(
        "open-loop steps remaining in {}: {total} (target 0)",
        game.name
    );
    let host = stage::host::run(&ctx, true);
    print_stage(&host);
    if !host.passed() {
        return Err("a host check failed: nothing blessed".into());
    }
    for (k, v) in &host.outcome.facts {
        if let Some(name) = k.strip_prefix("metric/") {
            base.host.insert(name.to_string(), v.parse().unwrap_or(0));
        }
    }
    let speed = stage::speed::run(&ctx, true);
    for w in &game.windows {
        if w.polls.is_none() {
            println!(
                "window {} is declared by conditions: not measurable yet, not blessed",
                w.name
            );
            continue;
        }
        let mut table = WindowBase {
            offsets: w.offsets.clone(),
            ..WindowBase::default()
        };
        for off in &w.offsets {
            let get = |m: &str| speed.outcome.facts.get(&format!("{}/{off}/{m}", w.name));
            let num = |m: &str| -> Result<u64, String> {
                get(m)
                    .and_then(|v| v.parse().ok())
                    .ok_or(format!("window {} offset {off}: no {m}", w.name))
            };
            table.flips.push(num("flips")?);
            table.work_per_frame.push(num("work_per_frame")?);
            table.frame_work_p99.push(num("frame_work_p99")?);
            table
                .vblanks
                .push(get("vblanks").cloned().unwrap_or_default());
        }
        base.speed.insert(w.name.clone(), table);
    }
    base.save(&o.repo)?;
    println!(
        "blessed {} into {} (size {:?}, {} windows, {} journeys)",
        util::short(&base.blessed_commit),
        game::BASELINE_FILE,
        base.size,
        base.speed.len(),
        base.journeys.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn print_stage(s: &StageResult) {
    for c in &s.outcome.checks {
        if c.status != Status::Pass {
            println!("  {:?} {}: {} {}", c.status, c.name, c.value, c.detail);
        }
    }
}

fn lint(o: &Options) -> Result<ExitCode, String> {
    let game = Game::load(&o.repo)?;
    let base = Baseline::load(&o.repo)?;
    let Some(spec) = &game.journeys else {
        println!("{}: no [journeys]", game.name);
        return Ok(ExitCode::SUCCESS);
    };
    let mut bad = 0;
    let mut total = 0;
    for path in stage::journeys::files(&o.repo, &spec.files)? {
        let info = journey::inspect(&path)?;
        total += info.open_loop;
        let pad = journey::check_pad(&info, &spec.forced_analog);
        let allowed = base
            .as_ref()
            .and_then(|b| b.journeys.get(&info.id))
            .map_or(0, |j| j.open_loop);
        let ratchet = info.open_loop <= allowed;
        if pad.is_err() || !ratchet {
            bad += 1;
        }
        println!(
            "{:<40} pad {:<10} open-loop {:>4} (baseline {allowed}){}{}",
            info.id,
            info.pad.as_deref().unwrap_or("MISSING"),
            info.open_loop,
            if ratchet { "" } else { "  OVER THE RATCHET" },
            pad.err().map_or(String::new(), |e| format!("  {e}"))
        );
    }
    if let Some(cross) = &game.cross {
        for s in &cross.smoke {
            for pad in &cross.pads {
                match journey::plan(&o.repo, &o.repo.join(s), pad, cross.min_distinct) {
                    Ok(p) => println!(
                        "smoke {s} {pad}: {} steps, {} frames budget",
                        p.steps.len(),
                        p.max_frames
                    ),
                    Err(e) => {
                        bad += 1;
                        println!("smoke {s} {pad}: {e}");
                    }
                }
            }
        }
    }
    println!("open-loop steps in total: {total} (target 0); journeys refused: {bad}");
    Ok(if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn install(o: &Options) -> Result<ExitCode, String> {
    let config = Config::load()?;
    let game = Game::load(&o.repo)?;
    let mut ctx = context(o, &game, &config)?;
    let build = stage::build::run(&mut ctx);
    if !build.passed() {
        print_stage(&build);
        return Err("the build failed".into());
    }
    let r = stage::install::run(&ctx);
    for c in &r.outcome.checks {
        println!("{:?} {} {} {}", c.status, c.name, c.value, c.detail);
    }
    Ok(if r.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
