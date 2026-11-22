//! Journeys on PSoXide: the pinned gate is the one runner. The harness lints every journey first
//! (explicit pad, the open-loop ratchet, a disc that a declared build produced), then runs one
//! gate process per journey and model, longest first, inside the emulator budget, with the
//! gate's own content receipts and the game's conformance rules.

use std::path::{Path, PathBuf};

use crate::journey::{self, Info};
use crate::report::{Check, Outcome, Source, StageResult, Status};
use crate::stage::{equiv, jreceipt, Ctx, Profile, Tier, FULL_MODELS};
use crate::{sched, util};

/// What one gate run printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    /// `--strict` runs: checkpoints the gate rendered on the hardware renderer at 1x and 3x
    /// (its `*.hw1.png` / `*.hw3.png` captures); `None` when unknown (a receipt never seen run).
    pub hw: Option<(u64, u64)>,
    /// Carried from this older gate revision (its `real` run reproduced exactly), not run now.
    pub carried_from: Option<String>,
    /// Answered by the harness's own journey receipt: no gate process started.
    pub harness_hit: bool,
    /// The gate's failed-check lines: what was checked, where, and the value it read.
    pub failures: Vec<String>,
    /// `AREAS <journey> <model> area=version ...`: the behaviour areas the run exercised.
    pub areas: std::collections::BTreeMap<String, String>,
    /// `DIGEST <journey> <model> <hex>`: what the run did.
    pub digest: Option<String>,
    /// `SIGNALS ...`: what the run met of the events the hardware models treat differently.
    pub signals: Option<super::equiv::Signals>,
    /// This row was not played: it stands on the `real` row (the signals that allowed it).
    pub stands_on: Option<String>,
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
        } else if let Some(rest) = l.strip_prefix("AREAS ") {
            // `AREAS <journey> <model> cpu=1 ...`: a model may hold `=` (gpu-irq=never).
            line.areas = rest
                .split_whitespace()
                .skip(2)
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        } else if let Some(rest) = l.strip_prefix("DIGEST ") {
            line.digest = rest.split_whitespace().nth(2).map(str::to_string);
        } else if let Some(rest) = l.strip_prefix("SIGNALS ") {
            line.signals = super::equiv::Signals::parse(rest);
        } else if let Some(rest) = l.strip_prefix("  FAIL  ") {
            // A failed check: `<journey>/<checkpoint or step>: <condition and the value read>`.
            line.failures.push(rest.trim().to_string());
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
    key.part("model", &job.label());
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
        line.hw = std::fs::read_to_string(path.with_extension("hw.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
    } else if line.passed {
        if let Some(hw) = line.hw {
            if let Ok(json) = serde_json::to_string(&hw) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path.with_extension("hw.json"), json);
            }
        }
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
    /// An extra row with the gate's pad override (`[journeys] pads`), else the journey's own pad.
    pad: Option<String>,
    expected_ticks: u64,
}

impl Job {
    /// The row's model part: `real`, or `real/digital` for a pad override row.
    fn label(&self) -> String {
        match &self.pad {
            Some(pad) => format!("{}/{pad}", self.model),
            None => self.model.clone(),
        }
    }
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
    let paths = match files(&ctx.repo, &spec.files) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            return result(
                Source::Run,
                Outcome::error(
                    "journeys",
                    format!("no file matches {}", spec.files.join(", ")),
                ),
            )
        }
        Err(e) => return result(Source::Run, Outcome::error("journeys", e)),
    };
    let expanded = match super::requires::expand(ctx, &paths, &spec.instances) {
        Ok(e) => e,
        Err(e) => return result(Source::Run, Outcome::error("journey instances", e)),
    };
    let instance_keys: std::collections::BTreeMap<PathBuf, String> = expanded
        .iter()
        .filter_map(|(p, k)| k.clone().map(|k| (p.clone(), k)))
        .collect();
    let mut checks = Vec::new();
    let mut facts = std::collections::BTreeMap::new();
    let mut runnable = Vec::new();
    let mut open_total = 0;
    let mut clock_total = 0;
    for path in expanded.iter().map(|(p, _)| p) {
        let info = match journey::inspect(path, &spec.clocks) {
            Ok(i) => i,
            Err(e) => {
                checks.push(Check::fail(path.display().to_string(), e));
                continue;
            }
        };
        open_total += info.open_loop;
        clock_total += info.clock_waits;
        facts.insert(format!("open_loop/{}", info.id), info.open_loop.to_string());
        facts.insert(
            format!("clock_waits/{}", info.id),
            info.clock_waits.to_string(),
        );
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
            // Baselines blessed before clock waits were counted do not enforce them; a journey
            // new to an existing baseline must have none.
            let clock_allowed = match base {
                Some(b) => b.clock_waits,
                None => ctx.baseline.as_ref().map(|_| 0),
            };
            if let Some(allowed) = clock_allowed {
                if info.clock_waits > allowed {
                    checks.push(Check::fail(
                        format!("{} closed-loop", info.id),
                        format!(
                            "{} waits on a free-running clock (an absolute vblank or present \
                             count is a fixed wait), the baseline allows {allowed}: wait on game \
                             state instead",
                            info.clock_waits
                        ),
                    ));
                    continue;
                }
            }
        }
        runnable.push((info, base.map_or(0, |b| b.ticks)));
    }
    checks.push(Check::new(
        "open-loop steps (ratchet, target 0)",
        Status::Pass,
        format!(
            "{} ({open_total} fixed waits or holds + {clock_total} waits on a free-running clock)",
            open_total + clock_total
        ),
    ));
    let models = models(ctx.tier, bless);
    let mut jobs = Vec::new();
    for (info, ticks) in runnable {
        // A template named in `precheck` stands for all of its instances.
        let prechecked = spec.precheck.iter().any(|p| {
            *p == info.id || spec.instances.contains(p) && info.id.starts_with(&format!("{p}-"))
        });
        if ctx.tier == Tier::Quick && !bless && !prechecked {
            continue;
        }
        for m in &models {
            if let Some(only) = spec.only_on.get(&info.id) {
                if !only.models.iter().any(|x| x == m) {
                    checks.push(Check::skip(
                        format!("{}@{m}", info.id),
                        format!("runs on {} only: {}", only.models.join(", "), only.reason),
                    ));
                    continue;
                }
            }
            if let Some(reason) = landing_skip(ctx.profile, m) {
                checks.push(Check::skip(format!("{}@{m}", info.id), reason));
                continue;
            }
            // One row with the journey's own pad, and one per extra pad the game file lists.
            let extra = spec.pads.get(&info.id).cloned().unwrap_or_default();
            for pad in std::iter::once(None).chain(extra.into_iter().map(Some)) {
                jobs.push(Job {
                    info: info.clone(),
                    model: (*m).to_string(),
                    pad,
                    expected_ticks: ticks,
                });
            }
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
    let receipts = ctx.work.join("gate-receipts");
    let width = ctx.config.budget.emulators;
    let shared = match jreceipt::Shared::read(ctx, &paths) {
        Ok(s) => s,
        Err(e) => return result(Source::Run, Outcome::error("journeys", e)),
    };
    let root = jreceipt::root(ctx);
    let runner = crate::config::GATE_REVISION;
    // The pinned gate's behaviour versions, once per stage (a gate without them: None).
    let versions = gate_versions(ctx, &gate);
    let play = |job: &Job| {
        ctx.timed(&format!("journey:{}@{}", job.info.id, job.label()), || {
            let render = if spec.hw && !spec.hw_off.contains_key(&job.info.id) {
                "--strict"
            } else {
                "--no-hw"
            };
            let mut flags = vec![render.to_string(), conformance.clone()];
            if let Some(pad) = &job.pad {
                flags.push("--pad".into());
                flags.push(pad.clone());
            }
            let golden = match (&spec.golden_dir, &job.info.name) {
                (Some(dir), Some(name)) => ctx.repo.join(dir.replace("{name}", name)),
                (None, Some(name)) => job
                    .info
                    .path
                    .parent()
                    .map_or_else(PathBuf::new, |p| p.join("golden").join(name)),
                _ => PathBuf::new(),
            };
            if spec.golden_dir.is_some() {
                flags.push("--golden-dir".into());
                flags.push(golden.display().to_string());
            }
            let key = match jreceipt::content_key(
                ctx,
                &shared,
                &job.info.path,
                instance_keys.get(&job.info.path).map(String::as_str),
                &job.info.discs,
                &golden,
                &flags,
            ) {
                Ok(k) => k,
                Err(e) => {
                    return (
                        Check::fail(format!("{}@{}", job.info.id, job.label()), e),
                        None,
                    )
                }
            };
            let at = jreceipt::path(&root, &key, runner, &job.label());
            let found = jreceipt::load(&at).or_else(|| {
                // A new gate that reports behaviour versions: carry any row whose exercised areas
                // it left alone. Otherwise `real` always runs again and another model is carried
                // only when `real` reproduced the older gate's run exactly (phase 1 ran every
                // `real`).
                let by_areas = versions
                    .as_ref()
                    .and_then(|v| jreceipt::carry_by_areas(&root, &key, runner, &job.label(), v));
                let carried = by_areas.or_else(|| {
                    (job.model != "real")
                        .then(|| jreceipt::carry(&root, &key, runner, &job.label()))
                        .flatten()
                })?;
                jreceipt::store(&at, &carried);
                Some(carried)
            });
            if let Some(record) = found {
                let name = format!("{}@{}", job.info.id, job.label());
                let how = match &record.carried_from {
                    Some(from) if versions.is_some() && !record.areas.is_empty() => format!(
                        ", CARRIED from gate {} (every area it exercised is unchanged: {})",
                        util::short(from),
                        record
                            .areas
                            .iter()
                            .map(|(a, v)| format!("{a}={v}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                    Some(from) => format!(
                        ", CARRIED from gate {} (real reproduced it exactly on this gate)",
                        util::short(from)
                    ),
                    None => ", receipt".into(),
                };
                println!("RECEIPT HIT journey {name} {}{how}", util::short(&key));
                let check = Check::pass(
                    &name,
                    format!(
                        "{} ticks, {}/{} checks{how}",
                        record.ticks, record.pass, record.checks
                    ),
                );
                return (check, Some(record.line()));
            }
            // The `real` re-proof after a gate change must be a full run: the gate's own receipt
            // would answer with a short summary that cannot show the run is the same.
            let mut run_flags = flags.clone();
            if job.model == "real" && jreceipt::older_exists(&root, &key, runner, &job.label()) {
                run_flags.push("--receipts-verify".into());
            }
            let (check, line) = one(ctx, &gate, job, &run_flags, &receipts);
            if let Some(l) = line
                .as_ref()
                .filter(|l| l.passed && check.status == Status::Pass)
            {
                jreceipt::store(&at, &jreceipt::Record::of(runner, l));
            }
            (check, line)
        })
    };
    // Phase 1 every `real` row, phase 2 the other models (which may carry from phase 1).
    let (first, second): (Vec<Job>, Vec<Job>) = jobs.into_iter().partition(|j| j.model == "real");
    let mut outcomes = sched::run_all(&first, width, play);
    // A row of another model stands on its journey's `real` row when the real run met none of
    // the events that model treats differently (equiv.rs); `--all-models` plays them all.
    let reals: std::collections::BTreeMap<(String, Option<String>), GateLine> = first
        .iter()
        .zip(&outcomes)
        .filter_map(|(job, (check, line))| {
            let line = line.as_ref().filter(|_| check.status == Status::Pass)?;
            Some(((job.info.id.clone(), job.pad.clone()), line.clone()))
        })
        .collect();
    let stand = |job: &Job| -> Option<(Check, Option<GateLine>)> {
        if ctx.all_models {
            return None;
        }
        let real = reals.get(&(job.info.id.clone(), job.pad.clone()))?;
        let signals = equiv::stands_on_real(&job.model, real).ok()?;
        let name = format!("{}@{}", job.info.id, job.label());
        println!("STANDS ON REAL journey {name}: {signals} observed");
        let line = GateLine {
            stands_on: Some(signals.clone()),
            receipt_hit: false,
            harness_hit: false,
            ..real.clone()
        };
        let check = Check::pass(
            &name,
            format!(
                "{} ticks, {}/{} checks, stands on real: {signals} observed (not run)",
                line.ticks, line.pass, line.checks
            ),
        );
        Some((check, Some(line)))
    };
    let dependent = |job: &Job| stand(job).unwrap_or_else(|| play(job));
    outcomes.extend(sched::run_all(&second, width, dependent));
    let jobs: Vec<Job> = first.into_iter().chain(second).collect();
    let mut hits = 0;
    let (mut harness_hits, mut gate_runs, mut gate_receipts) = (0usize, 0usize, 0usize);
    let mut sorted: Vec<(&Job, (Check, Option<GateLine>))> = jobs.iter().zip(outcomes).collect();
    sorted.sort_by(|a, b| (&a.0.info.id, a.0.label()).cmp(&(&b.0.info.id, b.0.label())));
    let mut hw_rows = (0usize, Vec::new(), Vec::new());
    let mut carried = Vec::new();
    let mut stood = Vec::new();
    for (job, (check, line)) in sorted {
        if let Some(signals) = line.as_ref().and_then(|l| l.stands_on.as_ref()) {
            stood.push(format!("{}@{}: {signals}", job.info.id, job.label()));
        }
        if let Some(from) = line.as_ref().and_then(|l| l.carried_from.as_ref()) {
            carried.push(format!(
                "{}@{} from {}",
                job.info.id,
                job.label(),
                util::short(from)
            ));
        }
        if let (true, false, Some(line)) = (spec.hw, spec.hw_off.contains_key(&job.info.id), &line)
        {
            let row = format!("{}@{}", job.info.id, job.label());
            match line.hw {
                Some((a, b)) if a > 0 && b > 0 => hw_rows.0 += 1,
                Some(_) => hw_rows.1.push(row),
                None => hw_rows.2.push(row),
            }
        }
        if let Some(line) = &line {
            hits += usize::from(line.receipt_hit);
            if line.stands_on.is_some() {
                // Neither a gate process nor a receipt: listed in `stood`.
            } else if line.harness_hit {
                harness_hits += 1;
            } else {
                gate_runs += 1;
                gate_receipts += usize::from(line.receipt_hit);
            }
            if job.model == "real" && job.pad.is_none() {
                facts.insert(format!("ticks/{}", job.info.id), line.ticks.to_string());
            }
            if line.skips_unknown {
                checks.push(Check::skip(
                    format!("{}@{} skips", job.info.id, job.label()),
                    "the gate answered from a receipt this harness never saw run, so what that \
                     run skipped is unknown here (run with fresh receipts to list it)",
                ));
            }
            if line.golden_skipped > 0 {
                checks.push(Check::skip(
                    format!("{}@{} golden", job.info.id, job.label()),
                    format!(
                        "{} golden compare(s) not run on this model (the journey's golden_on)",
                        line.golden_skipped
                    ),
                ));
            }
            if line.skip > 0 {
                checks.push(Check::skip(
                    format!("{}@{} conformance", job.info.id, job.label()),
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
    if spec.hw {
        // The table lists passing rows only for small stages, so the per-row evidence (each
        // row's value in the result file) is summed here where it is always printed.
        let (rendered, none, unknown) = hw_rows;
        let mut value = format!("{rendered} row(s) captured HW1 and HW3");
        if !none.is_empty() {
            value.push_str(&format!(
                "; no hardware capture (no picture checkpoint) in: {}",
                none.join(", ")
            ));
        }
        if !unknown.is_empty() {
            value.push_str(&format!(
                "; unknown (receipt never seen run) in: {}",
                unknown.join(", ")
            ));
        }
        checks.push(Check::pass("hardware renderer (hw = true)", value));
        for (id, reason) in &spec.hw_off {
            checks.push(Check::skip(
                format!("{id} hardware renderer"),
                format!("hw_off: {reason}"),
            ));
        }
    }
    // Always printed: where every row's verdict came from, so a fast run shows why it was fast.
    let model_skips = checks
        .iter()
        .filter(|c| c.status == Status::Skip && c.detail.starts_with("runs on "))
        .count();
    checks.push(Check::pass(
        "journey rows",
        format!(
            "{} row(s): {gate_runs} gate process(es) ({gate_receipts} answered by the gate's own \
             receipt), {} harness receipt(s), {} carried from an older gate, {} standing on real, \
             {model_skips} not run on a model (only_on)",
            jobs.len(),
            harness_hits - carried.len(),
            carried.len(),
            stood.len()
        ),
    ));
    if !stood.is_empty() {
        // Always printed: these rows were not played on their own model.
        checks.push(Check::pass(
            "standing on real (the real run met nothing these models treat differently; \
             --all-models plays them)",
            format!("{} row(s): {}", stood.len(), stood.join("; ")),
        ));
    }
    if !carried.is_empty() {
        // Always printed: these rows did not run on this gate.
        checks.push(Check::pass(
            "carried from an older gate (areas unchanged, or real re-proven exactly; not run now)",
            format!("{} row(s): {}", carried.len(), carried.join(", ")),
        ));
    }
    let source = match hits {
        0 => Source::Run,
        n if n == jobs.len() => Source::Hit,
        _ => Source::Mixed,
    };
    result(source, Outcome { checks, facts })
}

/// `psoxide-gate versions`, parsed; `None` when the gate has no such command.
/// The models a journey is listed on: `real` for bless and the quick tier, every full-tier model
/// for the full tier (whatever the profile: the landing profile lists the others as skips).
fn models(tier: Tier, bless: bool) -> Vec<&'static str> {
    match (tier, bless) {
        (_, true) | (Tier::Quick, false) => vec!["real"],
        (Tier::Full, false) => FULL_MODELS.to_vec(),
    }
}

/// Why the landing profile does not play `model` (every model but `real`); `None` = play it.
/// The row is a skip, never a pass: a landing result says nothing about that model.
fn landing_skip(profile: Profile, model: &str) -> Option<String> {
    (profile == Profile::Landing && model != "real").then(|| {
        format!(
            "not run on {model}: the landing profile plays journeys on real only (a landing \
             result is not an install result)"
        )
    })
}

fn gate_versions(ctx: &Ctx<'_>, gate: &Path) -> Option<std::collections::BTreeMap<String, String>> {
    let log = ctx
        .dir(&["journeys", "logs"])
        .ok()?
        .join("gate-versions.log");
    let argv = vec![gate.display().to_string(), "versions".to_string()];
    let done = util::run_logged(&argv, &ctx.repo, &[], &log, Some(60)).ok()?;
    (done.code == 0)
        .then(|| jreceipt::parse_versions(&done.output))
        .flatten()
}

/// The first failed checks, each with its condition and the value read (the gate's own words),
/// so the table says what broke without opening the log.
fn failures_note(failures: &[String]) -> String {
    const SHOWN: usize = 3;
    if failures.is_empty() {
        return String::new();
    }
    let mut note = format!(": {}", failures[..failures.len().min(SHOWN)].join("; "));
    if failures.len() > SHOWN {
        note.push_str(&format!(" (and {} more)", failures.len() - SHOWN));
    }
    note
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
    flags: &[String],
    receipts: &Path,
) -> (Check, Option<GateLine>) {
    let conformance = flags.join(" ");
    let name = format!("{}@{}", job.info.id, job.label());
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
        flags[0].clone(),
        "--hardware".into(),
        job.model.clone(),
        "--receipts".into(),
        receipts.display().to_string(),
        "--out".into(),
        out.display().to_string(),
    ]
    .into_iter()
    .chain(flags[1..].iter().cloned())
    .collect();
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = match ctx.machine("emu", || {
        util::run_logged(&argv, &ctx.repo, &env, &log, None)
    }) {
        Ok(d) => d,
        Err(e) => return (Check::fail(name, e), None),
    };
    let strict = flags[0] == "--strict";
    let mut parsed = parse_gate(&done.output);
    if let Some(line) = parsed.as_mut() {
        if strict && !line.receipt_hit {
            line.hw = Some(hw_captures(&out));
        }
        remember_skips(ctx, gate, job, &conformance, line);
    }
    let hw_note = |l: &GateLine| match (strict, l.hw) {
        (false, _) => String::new(),
        (true, Some((hw1, hw3))) => format!(", hardware renderer: {hw1} HW1 + {hw3} HW3 captures"),
        (true, None) => {
            ", hardware renderer: unknown (a receipt this harness never saw run)".into()
        }
    };
    let check = match &parsed {
        Some(l) if done.code == 0 && l.passed => Check::pass(
            &name,
            format!(
                "{} ticks, {}/{} checks{}{}",
                l.ticks,
                l.pass,
                l.checks,
                if l.receipt_hit { ", receipt" } else { "" },
                hw_note(l)
            ),
        )
        .detail(log.display().to_string()),
        Some(l) => Check::fail(
            &name,
            format!(
                "{} of {} checks failed{}{}{}; log {}",
                l.fail,
                l.checks,
                failures_note(&l.failures),
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

/// Count the hardware-renderer captures a `--strict` gate run wrote under `out`
/// (`<checkpoint>.hw1.png` and `<checkpoint>.hw3.png`; diff images are not counted).
fn hw_captures(out: &Path) -> (u64, u64) {
    let mut counts = (0, 0);
    let mut dirs = vec![out.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                dirs.push(path);
            } else if name.ends_with(".hw1.png") {
                counts.0 += 1;
            } else if name.ends_with(".hw3.png") {
                counts.1 += 1;
            }
        }
    }
    counts
}

/// Paths of the journeys a game file selects: every glob's matches, sorted, each once.
pub fn files(repo: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut all = Vec::new();
    for p in patterns {
        all.extend(util::glob(repo, p)?);
    }
    all.sort();
    all.dedup();
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_profile_plays_every_full_model_and_landing_only_real() {
        // No --profile is Install: the full tier's model list is exactly FULL_MODELS, as before.
        assert_eq!(models(Tier::Full, false), FULL_MODELS);
        assert_eq!(models(Tier::Quick, false), ["real"]);
        assert_eq!(models(Tier::Full, true), ["real"]);
        for m in FULL_MODELS {
            assert_eq!(landing_skip(Profile::Install, m), None);
        }
        assert_eq!(landing_skip(Profile::Landing, "real"), None);
        for m in &FULL_MODELS[1..] {
            let reason = landing_skip(Profile::Landing, m).expect("skipped under landing");
            assert!(reason.contains("landing profile"), "{reason}");
        }
    }

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
        let failed = "  FAIL  credits/end: ram CREDITS_STATE+0 eq 3 (read 1)\n";
        let l = parse_gate(&format!(
            "{failed}FAIL credits 7 checks: 6 pass, 1 fail | 9 ticks (0 s emulated) in 0.1 s | disc ab\n"
        ))
        .unwrap();
        assert_eq!(
            l.failures,
            ["credits/end: ram CREDITS_STATE+0 eq 3 (read 1)"]
        );
        assert!(failures_note(&l.failures).contains("(read 1)"));
        let out = "  ABORT  credits: step 3 timed out\nFAIL credits 7 checks: 4 pass, 3 fail, 2 golden skipped (hardware) | 536 ticks (9 s emulated) in 4.0 s | disc ab\n";
        let l = parse_gate(out).unwrap();
        assert!(!l.passed);
        assert_eq!((l.fail, l.golden_skipped, l.ticks), (3, 2, 536));
        assert_eq!(l.aborts, ["credits: step 3 timed out"]);
        assert!(parse_gate("no verdict").is_none());
    }
}
