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
mod scope;
mod stage;
mod tape;
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
  psoxide-test run [--repo DIR] [--tier quick|full] [--profile install|landing] [--stage NAME]... [--all-models] [--keep-going]
      the pipeline (stages, in run order: build size host stack speed journeys cross clean); stops
      a full tier at the first failed stage unless --keep-going; exit 0 only when every check that ran passed,
      and for --tier full only when the landing verdict is complete. --profile landing (full tier
      only, needs the owner's approval before lanes use it): journeys on real only, the speed
      bar's landing slack; never installable. No --profile is install: the full tier unchanged
  psoxide-test bless [--repo DIR] [--keep-speed [--trust-legacy-rows]]
      measure the reference build and write psoxide-test.baseline.toml (refuses a red journey);
      --keep-speed keeps the baseline's speed rows, refused unless a plain bless measured each on
      searched layouts (or none) and its window identity is unchanged
  psoxide-test lint [--repo DIR]
      load the game file and every journey; print the pad and open-loop findings
  psoxide-test tape --disc CUE --press SCHEDULE --out TAPE --steps N [--stop-at-poll P] [--pad analog|dualshock|digital]
      record a press schedule as a poll-bound tape and keep it only if it replays identically
  psoxide-test tape-anchor --tape IN --at-poll P --out OUT
      re-anchor a power-on tape at the poll a [[speed]] anchor holds at (refused if it presses before it)
  psoxide-test pin [--repo DIR] [--write] [--pinned REV] [--main REV] [--crates]
      the SDK pin rule for the game: its pinned commit against SDK main (or REV), over the SDK
      crates the game reaches (--crates lists them); --write records the pin's guest tree
  psoxide-test install [--repo DIR] [--rebase-facts] [--accept-slower TEXT]
      install the judged disc into the configured library (needs a complete clean full PASS on
      the install profile, a searched layout, never slower per speed window than the build
      installed before it, judged on the library's speed facts, which must describe the library's
      disc; no facts is a first install only when the library holds no disc of that name).
      --rebase-facts: when those facts cannot be compared, take this PASS as the new reference
      --accept-slower TEXT: the owner approved a build slower than the installed one; installs
      it with TEXT and each failed never-slower comparison, with its numbers, in the record
  psoxide-test install-check --result RESULT.json --repo GAME_CLONE --bin BIN [--map MAP]
                             (--facts FACTS.json --library-bin LIBRARY.bin | --first-install)
                             [--rebase-facts] [--accept-slower TEXT] [--write-facts OUT.json] [--stamp OUT.json]
      the same guarantee for an install made elsewhere: exit 0 only if RESULT (named by its own
      sha256) judged BIN and MAP, its committed layouts (read in GAME_CLONE at RESULT's commit)
      were searched, and it is never slower than the installed build whose speed facts are
      FACTS and whose disc is LIBRARY.bin. A missing FACTS is refused; --first-install says
      there is none. --write-facts writes the new speed facts; --stamp (on success only) writes
      the install marker a deploy script checks
config: ~/.config/psoxide-test/config.toml or PSOXIDE_TEST_CONFIG (see config.example.toml)";

const STAGES: &[&str] = &[
    "build", "requires", "size", "host", "journeys", "stack", "speed", "cross", "clean",
];

/// The order `run` plays the stages in: the cheap verdicts (size, host, stack, speed) before the
/// journeys, which are most of a full tier's wall time, so a run that is going to fail on a
/// cheap check says so in minutes. No stage reads another stage's output except through `build`.
const RUN_ORDER: &[&str] = &[
    "build", "requires", "size", "host", "stack", "speed", "journeys", "cross", "clean",
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
    /// `--all-models`: play every model of every journey, none standing on `real` (audit).
    all_models: bool,
    /// `--keep-going`: play every stage even after one has failed (the old behaviour). By default
    /// a full tier stops at its first failed stage and lists the rest as not run.
    keep_going: bool,
    /// `--profile`: the full tier's gates (stage::Profile); `Install` when not given.
    profile: stage::Profile,
    /// `--rebase-facts` (install only): when the installed build's speed facts cannot be compared
    /// (another window identity, facts of another disc, or legacy facts), take this strict PASS
    /// as the new reference instead of refusing. Never waives a comparison that can be made.
    rebase_facts: bool,
    /// `--accept-slower TEXT` (install only): the owner's approval of a build slower than the
    /// installed one, recorded with the measured slowdown in the install record.
    accept_slower: Option<String>,
    /// `bless --keep-speed`: keep the baseline's speed rows (light landings on a greedy layout).
    keep_speed: bool,
    /// `bless --keep-speed --trust-legacy-rows`: adopt a pre-provenance baseline's rows once.
    trust_legacy: bool,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        repo: std::env::current_dir().map_err(|e| e.to_string())?,
        tier: Tier::Quick,
        stages: Vec::new(),
        all_models: false,
        keep_going: false,
        keep_speed: false,
        trust_legacy: false,
        profile: stage::Profile::Install,
        rebase_facts: false,
        accept_slower: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--repo" => o.repo = PathBuf::from(value()?),
            "--all-models" => o.all_models = true,
            "--keep-going" => o.keep_going = true,
            "--keep-speed" => o.keep_speed = true,
            "--trust-legacy-rows" => o.trust_legacy = true,
            "--rebase-facts" => o.rebase_facts = true,
            "--accept-slower" => {
                o.accept_slower = Some(stage::install::approval_text(&value()?)?);
            }
            "--profile" => {
                o.profile = match value()?.as_str() {
                    "install" => stage::Profile::Install,
                    "landing" => stage::Profile::Landing,
                    p => return Err(format!("unknown profile `{p}` (install or landing)")),
                }
            }
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
            check_profile(&o, true)?;
            run(&no_rebase(o)?)
        }
        Some("bless") => bless(&no_rebase(checked(&args[1..])?)?),
        Some("lint") => lint(&no_rebase(checked(&args[1..])?)?),
        Some("install") => install(&checked(&args[1..])?),
        Some("install-check") => stage::install::check_command(&args[1..]),
        Some("adapter") => adapter(&args[1..]),
        Some("pin") => pin_command(&args[1..]),
        Some("tape") => tape::run(&args[1..]),
        Some("tape-anchor") => tape::anchor_command(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

/// `options` for a command other than `run`, which takes no landing profile.
/// `--rebase-facts` is an `install` option only.
fn no_rebase(o: Options) -> Result<Options, String> {
    if o.rebase_facts || o.accept_slower.is_some() {
        Err("--rebase-facts and --accept-slower are `install` options only".into())
    } else {
        Ok(o)
    }
}

fn checked(args: &[String]) -> Result<Options, String> {
    let o = options(args)?;
    check_profile(&o, false)?;
    Ok(o)
}

/// `--profile landing` is a full-tier `run` option only, and not an audit (`--all-models`).
fn check_profile(o: &Options, is_run: bool) -> Result<(), String> {
    if o.profile == stage::Profile::Install {
        return Ok(());
    }
    if !is_run {
        return Err("--profile landing is a `run --tier full` option only".into());
    }
    if o.tier != Tier::Full {
        return Err("--profile landing needs --tier full".into());
    }
    if o.all_models {
        return Err("--profile landing plays journeys on real only: drop --all-models".into());
    }
    Ok(())
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
        profile: o.profile,
        all_models: o.all_models,
        rebase_facts: o.rebase_facts,
        accept_slower: o.accept_slower.clone(),
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

/// A stage the run never started because an earlier stage failed.
fn stopped_at(name: &str, failed: &str) -> StageResult {
    StageResult {
        name: name.into(),
        source: Source::NotRun,
        key: String::new(),
        outcome: Outcome {
            checks: vec![Check::fail(
                name,
                format!("not run: the run stopped at `{failed}`, which failed (--keep-going plays every stage)"),
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
    // The first failed stage, once the run stops at it. Only a full tier stops: a quick tier is
    // already cheap, and repin flows run it expecting a size or speed red before they bless.
    // `--keep-going` never stops. A failed SDK pin does not stop the run: it judges the pin, not
    // these bytes, and a lane may prove its bytes on an older pin on purpose.
    let mut stopped: Option<String> = None;
    stages.push(sdk);
    // A full tier whose clean clone cannot fetch this commit fails at its very end, after every
    // journey: ask GitHub first. Only a definite "not there" stops the run.
    if stopped.is_none() && o.tier == Tier::Full && wanted("clean") && !game.cleans.is_empty() {
        let pre = ctx.timed("pushed", || stage::clean::pushed_precheck(&ctx));
        if !pre.passed() && !o.keep_going {
            stopped = Some("pushed".into());
        }
        stages.push(pre);
    }
    let t0 = util::now_seconds();
    let built = if let Some(at) = &stopped {
        stages.push(stopped_at("build", at));
        false
    } else {
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
        built
    };
    for name in &RUN_ORDER[1..] {
        if !wanted(name) {
            continue;
        }
        if optional_stage_absent(name, &game) {
            continue;
        }
        if let Some(at) = &stopped {
            stages.push(stopped_at(name, at));
            continue;
        }
        if !built {
            stages.push(not_run(name, "build"));
            continue;
        }
        let r = ctx.timed(name, || match *name {
            "requires" => stage::requires::run(&ctx),
            "stack" => stage::stack::run(&ctx, false),
            "size" => stage::size::run(&ctx),
            "host" => stage::host::run(&ctx, false),
            "journeys" => stage::journeys::run(&ctx, false),
            "speed" => stage::speed::run(&ctx, false),
            "cross" => stage::cross::run(&ctx),
            "clean" => stage::clean::run(&ctx),
            _ => unreachable!("RUN_ORDER lists only these"),
        });
        if !r.passed() && !o.keep_going && o.tier == Tier::Full {
            stopped = Some(name.to_string());
        }
        stages.push(r);
    }
    if let Some(at) = &stopped {
        println!(
            "STOPPED at the first failed stage ({at}); the stages after it did not run \
             (--keep-going plays them all)"
        );
    }
    let mut missing = Vec::new();
    if o.tier != Tier::Full {
        missing.push("tier full".to_string());
    }
    for s in STAGES {
        if optional_stage_absent(s, &game) {
            continue;
        }
        if !stages.iter().any(|r| r.name == *s) {
            missing.push(format!("stage {s}"));
        }
    }
    for (section, declared) in [
        ("[[size]]", !game.sizes.is_empty()),
        (
            "[[speed]] (or a [[host]] metric)",
            // A disc image inherits its members' speed verdicts through [[requires]].
            !game.windows.is_empty()
                || game.hosts.iter().any(|h| h.metric.is_some())
                || !game.requires.is_empty(),
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
    let dirty = tree_dirty(&o.repo);
    let mut result = RunResult {
        schema: report::result_schema(o.profile),
        game: game.name.clone(),
        commit,
        dirty,
        sdk_pin: stage::sdk_pin(&o.repo, &game),
        sdk_guest,
        harness: env!("CARGO_PKG_VERSION").into(),
        gate_revision: GATE_REVISION.into(),
        tier: o.tier.name().into(),
        profile: o.profile,
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

/// A stage that only runs when the game file declares it (`[[requires]]`, `[[stack]]`).
fn optional_stage_absent(stage: &str, game: &Game) -> bool {
    match stage {
        "requires" => game.requires.is_empty(),
        "stack" => game.stacks.is_empty(),
        _ => false,
    }
}

fn ctx_tier_has(tier: Tier, stage: &str) -> bool {
    match tier {
        Tier::Full => true,
        Tier::Quick => matches!(
            stage,
            "build" | "requires" | "size" | "journeys" | "speed" | "cross"
        ),
    }
}

fn bless(o: &Options) -> Result<ExitCode, String> {
    // A --keep-speed bless serves a light landing: the game's host checks get the landing
    // allowance on their own layout-sensitive ceilings, as `run --profile landing` gives them.
    let landing;
    let o = if o.keep_speed {
        landing = Options {
            profile: stage::Profile::Landing,
            repo: o.repo.clone(),
            stages: o.stages.clone(),
            accept_slower: None,
            ..*o
        };
        &landing
    } else {
        o
    };
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
            commit.clone()
        },
        sdk_pin: stage::sdk_pin(&o.repo, &game),
        ..Baseline::default()
    };
    for size in &game.sizes {
        let map = size
            .map_path(&game, &o.repo)
            .ok_or("[size] build has no map")?;
        let text =
            std::fs::read_to_string(&map).map_err(|e| format!("read {}: {e}", map.display()))?;
        base.size.insert(size.key(), stage::size::measure(&text)?);
        // Kept under its digest so a later size change can be explained by function.
        base.maps
            .insert(size.key(), stage::size::keep_map(&ctx, &map)?);
    }
    // The game's host checks first: they take minutes, the journeys most of a bless, and a host
    // refusal stops the bless either way.
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
    if !game.stacks.is_empty() {
        let stack = stage::stack::run(&ctx, true);
        print_stage(&stack);
        if !stack.passed() {
            return Err("the stack stage failed, nothing blessed".into());
        }
        for (k, v) in &stack.outcome.facts {
            if let Some(id) = k.strip_prefix("stack/") {
                base.stack.insert(id.to_string(), v.parse().unwrap_or(0));
            }
        }
    }
    if o.keep_speed {
        // Light landings (owner, 11-22): a greedy-layout build must not become the speed reference,
        // or each landing's slack would stack on the last. Keep the rows the previous bless wrote
        // (a searched layout) and measure nothing.
        // The kept rows must come from a plain bless on searched layouts and still measure what
        // the game file and config declare; their records travel with them unchanged.
        let old = ctx
            .baseline
            .as_ref()
            .ok_or("--keep-speed needs a baseline: run a plain bless on a searched layout")?;
        let now = stage::install::window_identities(&o.repo, &commit, &game, GATE_REVISION)?;
        base.speed = baseline::kept_speed(old, &now, o.trust_legacy)?;
        // Load rows are speed rows too: kept, not re-measured.
        base.load = old.load.clone();
        if o.trust_legacy {
            println!(
                "TRUSTED LEGACY SPEED ROWS from blessed commit {} as the landing reference \
                 (--trust-legacy-rows); installs still need a stamped search, a plain bless and \
                 never-slower against the library",
                util::short(&old.blessed_commit)
            );
        }
        base.save(&o.repo)?;
        println!(
            "blessed {} into {} KEEPING the previous speed rows ({} windows, not re-measured; \
             --keep-speed); size {:?}, {} journeys",
            util::short(&base.blessed_commit),
            game::BASELINE_FILE,
            base.speed.len(),
            base.size,
            base.journeys.len()
        );
        return Ok(ExitCode::SUCCESS);
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
    let rows: Vec<(String, &game::Window)> = game
        .windows
        .iter()
        .flat_map(|w| w.rows().into_iter().map(move |(row, _)| (row, w)))
        .collect();
    // What each row measured, recorded so `--keep-speed` can later prove the rows it keeps came
    // from a searched layout and still measure the same thing. A record that cannot be read
    // (a tape or layout the commit does not hold) is left out, with a warning: the row is then
    // never kept.
    let identities = stage::install::window_identities(&o.repo, &commit, &game, GATE_REVISION)
        .map_err(|e| eprintln!("warning: no window identity recorded: {e}"))
        .ok();
    for (row, w) in rows {
        let layouts = game
            .build(&w.build)
            .map(|b| layout_records(&o.repo, &commit, b));
        let layouts = match layouts {
            Some(Ok(l)) => Some(l),
            Some(Err(e)) => {
                eprintln!("warning: no layout provenance recorded for `{row}`: {e}");
                None
            }
            None => None,
        };
        let mut table = WindowBase {
            offsets: w.offsets.clone(),
            layouts,
            identity: identities.as_ref().and_then(|i| i.get(&row).cloned()),
            ..WindowBase::default()
        };
        for off in &w.offsets {
            let get = |m: &str| speed.outcome.facts.get(&format!("{row}/{off}/{m}"));
            let num = |m: &str| -> Result<u64, String> {
                get(m)
                    .and_then(|v| v.parse().ok())
                    .ok_or(format!("window {row} offset {off}: no {m}"))
            };
            table.flips.push(num("flips")?);
            table.work_per_frame.push(num("work_per_frame")?);
            table.frame_work_p99.push(num("frame_work_p99")?);
            table
                .vblanks
                .push(get("vblanks").cloned().unwrap_or_default());
        }
        base.speed.insert(row, table);
    }
    for l in &game.loads {
        let num = |m: &str| -> Result<u64, String> {
            speed
                .outcome
                .facts
                .get(&format!("load/{}/{m}", l.name))
                .and_then(|v| v.parse().ok())
                .ok_or(format!("load {}: no {m}", l.name))
        };
        base.load.insert(
            l.name.clone(),
            stage::load::LoadBase {
                vblanks: num("vblanks")?,
                cycles: num("cycles")?,
            },
        );
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

/// The layout profiles `build` links at `commit`, recorded for the baseline: path, SHA-256 and
/// search stamp.
fn layout_records(
    repo: &Path,
    commit: &str,
    build: &game::Build,
) -> Result<Vec<baseline::LayoutRecord>, String> {
    stage::install::committed_layouts(repo, commit, build)?
        .into_iter()
        .map(|l| {
            Ok(baseline::LayoutRecord {
                sha256: util::git_blob_sha256(repo, commit, &l.path)?,
                searched: stage::install::search_stamp(&l.text)
                    .map(|(rounds, seed)| baseline::SearchStamp { rounds, seed }),
                path: l.path,
            })
        })
        .collect()
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
    let is_template = |p: &Path| {
        p.file_stem()
            .is_some_and(|s| spec.instances.iter().any(|t| t.as_str() == s))
    };
    for path in stage::journeys::files(&o.repo, &spec.files)? {
        let info = if is_template(&path) {
            // Played once per [[requires]] member; lint reads it with a stand-in member.
            let copy = stage::requires::lint_copy(&path)?;
            let info = journey::inspect(&copy, &spec.clocks);
            let _ = std::fs::remove_dir_all(copy.parent().unwrap_or(&copy));
            info?
        } else {
            journey::inspect(&path, &spec.clocks)?
        };
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
            let path = o.repo.join(s);
            if is_template(&path) {
                println!(
                    "smoke {s}: a template, planned per [[requires]] member once the image's \
                     members.sym exists (a run)"
                );
                continue;
            }
            for pad in &cross.pads {
                match journey::plan(&o.repo, &path, pad, cross.min_distinct) {
                    Ok(p) => println!(
                        "smoke {s} {pad}: {} steps, {} frames budget",
                        p.steps.len(),
                        p.max_frames
                    ),
                    // Symbols come from a build: before the first build that is not a defect.
                    Err(e) if e.contains("build the artifact that writes it first") => {
                        println!("smoke {s} {pad}: not planned yet, its symbols are not built");
                    }
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
    let verdict = match pin::judge(work_root, &pin, &scope::of_game(repo), None) {
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
    let mut value = format!(
        "{} guest tree {} {} main {} guest tree {} (compared: {})",
        util::short(&verdict.pinned),
        util::short(&verdict.pinned_tree),
        if verdict.pinned_tree == verdict.main_tree {
            "equals"
        } else {
            "DIFFERS FROM"
        },
        util::short(&verdict.main),
        util::short(&verdict.main_tree),
        verdict.scope
    );
    if let Some(age) = verdict.main_age {
        value.push_str(&format!(
            " (offline: main as fetched {} ago)",
            pin::age_text(age)
        ));
    }
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
        .filter(|r| **r != verdict.pinned_whole)
    {
        checks.push(Check::fail(
            "sdk guest_tree recorded",
            format!(
                "the lock records {} but {} has {}: run `psoxide-test pin --write`",
                util::short(recorded),
                util::short(&verdict.pinned),
                util::short(&verdict.pinned_whole)
            ),
        ));
    }
    debug_assert_eq!(
        verdict.holds(),
        checks.iter().all(|c| c.status == Status::Pass)
    );
    (stage(checks), verdict.header())
}

/// `psoxide-test pin [--repo DIR] [--write] [--pinned REV] [--main REV] [--crates]`: apply the pin
/// rule and print it (`--crates`: which SDK crates of main the game reaches); `--write` records the pinned commit's guest tree in the lock (components.lock.json or
/// sdk.lock.json). `--pinned` and `--main` judge other commits than the lock's and SDK main (would
/// this game have to repin if main moved to REV?); they cannot be written.
fn pin_command(args: &[String]) -> Result<ExitCode, String> {
    let write = args.iter().any(|a| a == "--write");
    let crates = args.iter().any(|a| a == "--crates");
    let (mut pinned, mut main, mut rest) = (None, None, Vec::new());
    let mut it = args.iter().filter(|a| *a != "--write" && *a != "--crates");
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pinned" => pinned = Some(it.next().ok_or("--pinned needs a value")?.clone()),
            "--main" => main = Some(it.next().ok_or("--main needs a value")?.clone()),
            _ => rest.push(a.clone()),
        }
    }
    if write && (pinned.is_some() || main.is_some()) {
        return Err("--write records the lock's own pin: drop --pinned and --main".into());
    }
    let o = options(&rest)?;
    let config = Config::load()?;
    let game = Game::load(&o.repo)?;
    let mut pin = pin::read(&o.repo, Some(&game))
        .ok_or("no SDK pin in components.lock.json, sdk.lock.json or the game file's [sdk]")?;
    if let Some(rev) = pinned {
        pin.revision = rev;
        pin.recorded = None;
    }
    let game_scope = scope::of_game(&o.repo);
    let v = pin::judge(&config.work, &pin, &game_scope, main.as_deref())?;
    if crates {
        match &game_scope {
            scope::Scope::Game(reach) => {
                let mirror = pin::Mirror::open(&config.work, &pin.repository)?;
                let reached = mirror.reached(&v.main, reach)?;
                for dir in &reached.crates {
                    let mark = if reached.reached.contains(dir) {
                        "reached"
                    } else {
                        "out"
                    };
                    println!("{mark} {dir}");
                }
            }
            scope::Scope::Whole(why) => println!("every crate: {why}"),
        }
    }
    println!(
        "{}: pinned {} guest tree {} (whole {})\nSDK main {} guest tree {} (whole {})\ncompared: {}\n{}",
        v.file,
        v.pinned,
        v.pinned_tree,
        v.pinned_whole,
        v.main,
        v.main_tree,
        v.main_whole,
        v.scope,
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
                v.file, v.pinned_whole
            ));
        };
        let new = pin::record_guest_tree(&text, section, &v.pinned_whole)
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

/// Whether the repo's tracked files differ from HEAD. A git that cannot answer (not a repository,
/// git missing) counts as dirty: the clean-tree refusal must never pass on an error.
fn tree_dirty(repo: &Path) -> bool {
    util::git(repo, &["status", "--porcelain", "--untracked-files=no"])
        .map_or(true, |status| !status.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_git_reads_as_dirty() {
        let dir = tempfile::tempdir().unwrap();
        // Not a repository: git fails, and that is dirty, never clean.
        assert!(tree_dirty(&dir.path().join("missing")));
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("f"), "a").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "c"]);
        assert!(!tree_dirty(dir.path()));
        std::fs::write(dir.path().join("f"), "b").unwrap();
        assert!(tree_dirty(dir.path()));
    }

    #[test]
    fn only_landing_results_carry_schema_2() {
        assert_eq!(report::result_schema(stage::Profile::Install), 1);
        assert_eq!(report::result_schema(stage::Profile::Landing), 2);
        assert_eq!(report::RESULT_SCHEMA_MAX, 2);
    }

    fn parse(args: &[&str]) -> Result<Options, String> {
        let dir = std::env::temp_dir().display().to_string();
        let mut all: Vec<String> = vec!["--repo".into(), dir];
        all.extend(args.iter().map(|s| s.to_string()));
        options(&all)
    }

    #[test]
    fn no_profile_is_the_install_profile() {
        let o = parse(&["--tier", "full"]).unwrap();
        assert_eq!(o.profile, stage::Profile::Install);
        assert!(check_profile(&o, true).is_ok());
        let explicit = parse(&["--tier", "full", "--profile", "install"]).unwrap();
        assert_eq!(explicit.profile, o.profile);
    }

    #[test]
    fn landing_is_a_full_tier_run_option_only() {
        let o = parse(&["--tier", "full", "--profile", "landing"]).unwrap();
        assert_eq!(o.profile, stage::Profile::Landing);
        assert!(check_profile(&o, true).is_ok());
        assert!(
            check_profile(&o, false).is_err(),
            "bless and install refuse it"
        );
        let quick = parse(&["--profile", "landing"]).unwrap();
        assert!(check_profile(&quick, true).is_err());
        let audit = parse(&["--tier", "full", "--profile", "landing", "--all-models"]).unwrap();
        assert!(check_profile(&audit, true).is_err());
        assert!(parse(&["--profile", "fast"]).is_err());
    }
}
