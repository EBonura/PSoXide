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
mod pin;
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
        Some("pin") => pin_command(&args[1..]),
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
    if let Some(problem) = stage::speed::pgo_source_problem(&o.repo, &game, &config) {
        return Err(problem);
    }
    let mut ctx = context(o, &game, &config)?;
    let wanted = |s: &str| {
        let in_tier = ctx_tier_has(o.tier, s);
        in_tier && (o.stages.is_empty() || o.stages.iter().any(|x| x == s))
    };
    let mut stages = Vec::new();
    // The SDK pin rule first: it is not a receipt (SDK main moves), and a run that cannot
    // show it is not a landing.
    let (sdk, sdk_guest) = ctx.timed("sdk", || sdk_stage(&o.repo, &game, config.work.as_path()));
    stages.push(sdk);
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
        ("[[clean]]", !game.cleans.is_empty()),
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
        sdk_pin: stage::sdk_pin(&o.repo, &game),
        sdk_guest,
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
    if let Some(problem) = stage::speed::pgo_source_problem(&o.repo, &game, &config) {
        return Err(problem);
    }
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
        sdk_pin: stage::sdk_pin(&o.repo, &game),
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
        // Kept under its digest so a later size change can be explained by function.
        base.maps
            .insert(size.build.clone(), stage::size::keep_map(&ctx, &map)?);
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
        } else if let Some(id) = k.strip_prefix("clock_waits/") {
            let e: &mut JourneyBase = base.journeys.entry(id.to_string()).or_default();
            e.clock_waits = Some(v.parse().unwrap_or(0));
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
            // Clock waits counted for the first time are recorded as they are; after that they
            // only go down.
            let clock_allowed = old.journeys.get(id).and_then(|j| j.clock_waits);
            if let (Some(allowed), Some(now)) = (clock_allowed, now.clock_waits) {
                if now > allowed {
                    raised.push(format!("{id} clock waits {allowed} -> {now}"));
                }
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
    let clock: u32 = base.journeys.values().filter_map(|j| j.clock_waits).sum();
    println!(
        "open-loop steps remaining in {}: {} ({total} fixed waits or holds + {clock} waits on a \
         free-running clock; target 0)",
        game.name,
        total + clock
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
    // Before reading the windows: a stage that could not measure says why (not "no flips").
    let broken: Vec<&Check> = speed
        .outcome
        .checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .filter(|c| {
            !c.name.ends_with(" baseline")
                && ![" frames presented", " cycles per frame"]
                    .iter()
                    .any(|m| c.name.contains(m))
        })
        .collect();
    if let Some(first) = broken.first() {
        for c in &broken {
            println!("  FAIL {}: {} {}", c.name, c.value, c.detail);
        }
        return Err(format!(
            "the speed stage failed, nothing blessed: {}: {}",
            first.name, first.detail
        ));
    }
    for w in &game.windows {
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
    if let Some(problem) = Config::load()
        .ok()
        .and_then(|c| stage::speed::pgo_source_problem(&o.repo, &game, &c))
    {
        println!("REFUSED (run and bless stop here): {problem}");
    }
    let Some(spec) = &game.journeys else {
        println!("{}: no [journeys]", game.name);
        return Ok(ExitCode::SUCCESS);
    };
    let mut bad = 0;
    let mut total = 0;
    for path in stage::journeys::files(&o.repo, &spec.files)? {
        let info = journey::inspect(&path, &spec.clocks)?;
        total += info.open_loop + info.clock_waits;
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
            "{:<40} pad {:<10} open-loop {:>4} (baseline {allowed}) clock waits {:>3}{}{}",
            info.id,
            info.pad.as_deref().unwrap_or("MISSING"),
            info.open_loop,
            info.clock_waits,
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
    println!(
        "open-loop steps in total: {total}, waits on a free-running clock included (target 0); \
         journeys refused: {bad}"
    );
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

/// The SDK pin rule (pin.rs) as a stage: the pinned commit's guest tree against SDK main's.
/// Returns the stage and the line the result header shows.
fn sdk_stage(repo: &Path, game: &Game, work_root: &Path) -> (StageResult, String) {
    let stage = |checks: Vec<Check>| StageResult {
        name: "sdk".into(),
        source: Source::Run,
        key: String::new(),
        outcome: Outcome {
            checks,
            ..Outcome::default()
        },
    };
    let Some(pin) = pin::read(repo, Some(game)) else {
        return (
            stage(vec![Check::skip(
                "sdk pin",
                "no pin found in components.lock.json, sdk.lock.json or the game file's [sdk]",
            )]),
            "SDK pin unknown".into(),
        );
    };
    let verdict = match pin::judge(work_root, &pin) {
        Ok(v) => v,
        Err(e) => {
            return (
                stage(vec![Check::fail(
                    format!("sdk pin ({})", pin.file),
                    format!("cannot apply the pin rule: {e}"),
                )]),
                "guest tree not checked".into(),
            )
        }
    };
    let name = format!("sdk pin ({})", verdict.file);
    let value = format!(
        "{} guest tree {} {} main {} guest tree {}",
        util::short(&verdict.pinned),
        util::short(&verdict.pinned_tree),
        if verdict.pinned_tree == verdict.main_tree {
            "equals"
        } else {
            "DIFFERS FROM"
        },
        util::short(&verdict.main),
        util::short(&verdict.main_tree)
    ) + &verdict.main_age.map_or(String::new(), |a| {
        format!(" (offline: main as fetched {} ago)", pin::age_text(a))
    });
    let mut checks = vec![if verdict.pinned_tree == verdict.main_tree {
        Check::pass(&name, value)
    } else {
        Check::new(&name, Status::Fail, value).detail(
            "SDK main changed guest content since this pin: repin to main (a host-tool-only \
             commit never does this)",
        )
    }];
    if let Some(recorded) = verdict
        .recorded
        .as_ref()
        .filter(|r| **r != verdict.pinned_tree)
    {
        checks.push(Check::fail(
            "sdk guest_tree recorded",
            format!(
                "the lock records {} but {} has {}: run `psoxide-test pin --write`",
                util::short(recorded),
                util::short(&verdict.pinned),
                util::short(&verdict.pinned_tree)
            ),
        ));
    }
    debug_assert_eq!(
        verdict.holds(),
        checks.iter().all(|c| c.status == Status::Pass)
    );
    (stage(checks), verdict.header())
}

/// `psoxide-test pin [--repo DIR] [--write]`: apply the pin rule and print it; `--write` records
/// the pinned commit's guest tree in the lock (components.lock.json or sdk.lock.json).
fn pin_command(args: &[String]) -> Result<ExitCode, String> {
    let write = args.iter().any(|a| a == "--write");
    let rest: Vec<String> = args.iter().filter(|a| *a != "--write").cloned().collect();
    let o = options(&rest)?;
    let config = Config::load()?;
    let game = Game::load(&o.repo)?;
    let pin = pin::read(&o.repo, Some(&game))
        .ok_or("no SDK pin in components.lock.json, sdk.lock.json or the game file's [sdk]")?;
    let v = pin::judge(&config.work, &pin)?;
    println!(
        "{}: pinned {} guest tree {}\nSDK main {} guest tree {}\n{}",
        v.file,
        v.pinned,
        v.pinned_tree,
        v.main,
        v.main_tree,
        v.header()
    );
    if write {
        let path = o.repo.join(&v.file);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let section = if v.file == "components.lock.json" {
            "\"sdk\""
        } else if v.file == "sdk.lock.json" {
            "{"
        } else {
            return Err(format!(
                "the pin is in {}: add `guest_tree = \"{}\"` under [sdk] in the game file",
                v.file, v.pinned_tree
            ));
        };
        let new = pin::record_guest_tree(&text, section, &v.pinned_tree)
            .ok_or_else(|| format!("cannot find the SDK revision in {}", path.display()))?;
        std::fs::write(&path, new).map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("recorded guest_tree in {}", v.file);
    }
    Ok(if v.holds() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
