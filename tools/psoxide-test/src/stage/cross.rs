//! The cross-emulator tier: each smoke journey, resolved against the build that made its disc,
//! replayed with every declared pad on PCSX-ReARMed (quick and full) and PCSX-Redux (full).
//! A run passes when every checkpoint's RAM conditions hold, no step times out, the emulator did
//! not pause or crash, and at least `min_distinct` distinct pictures were presented. Pixels are
//! never compared across emulators.

use std::path::{Path, PathBuf};

use crate::journey::{self, Plan};
use crate::rearmed::Replay;
use crate::report::{Check, Outcome, Source, StageResult};
use crate::sched;
use crate::stage::{Ctx, Tier};
use crate::util::{self, Key};

/// The other emulators the tier drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emulator {
    Rearmed,
    Redux,
}

impl Emulator {
    fn name(self) -> &'static str {
        match self {
            Emulator::Rearmed => "rearmed",
            Emulator::Redux => "redux",
        }
    }
}

struct Job {
    emulator: Emulator,
    plan: Plan,
    smoke: String,
    tool_digest: String,
    disc_digest: String,
}

/// The cross stage.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let result = |source, outcome| StageResult {
        name: "cross".into(),
        source,
        key: String::new(),
        outcome,
    };
    let Some(cross) = &ctx.game.cross else {
        return result(
            Source::Run,
            Outcome {
                checks: vec![Check::skip("cross", "the game file has no [cross]")],
                ..Outcome::default()
            },
        );
    };
    let emulators: &[Emulator] = match ctx.tier {
        Tier::Quick => &[Emulator::Rearmed],
        Tier::Full => &[Emulator::Rearmed, Emulator::Redux],
    };
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return result(Source::Run, Outcome::error("cross", e.to_string())),
    };
    let mut checks = Vec::new();
    let mut jobs = Vec::new();
    for emulator in emulators {
        let tool = match emulator {
            Emulator::Rearmed => ctx
                .config
                .tool(&ctx.config.tools.rearmed_core, "[tools] rearmed_core")
                .map(Path::to_path_buf)
                .and_then(|core| {
                    Ok(format!(
                        "{} {}",
                        util::sha256_file(&core)?,
                        util::sha256_file(&self_exe)?
                    ))
                }),
            Emulator::Redux => ctx
                .config
                .tool(&ctx.config.tools.redux, "[tools] redux")
                .and_then(|c| util::sha256_file(&c.join("MacOS/PCSX-Redux")))
                .map(|d| {
                    format!(
                        "{d} {}",
                        util::sha256_bytes(crate::redux::DRIVER.as_bytes())
                    )
                }),
        };
        let tool_digest = match tool {
            Ok(d) => d,
            Err(e) => {
                checks.push(Check::fail(emulator.name(), e));
                continue;
            }
        };
        if let Some(boot) = &cross.boot {
            let Some(b) = ctx.game.build(&boot.build) else {
                checks.push(Check::fail(
                    "boot",
                    format!("unknown build `{}`", boot.build),
                ));
                continue;
            };
            let disc_digest = ctx
                .artifacts
                .get(&b.name)
                .and_then(|a| a.get(&b.disc.replace(".cue", ".bin")))
                .cloned()
                .unwrap_or_default();
            jobs.push(Job {
                emulator: *emulator,
                plan: Plan {
                    name: format!("boot-{}", b.name),
                    disc: b.cue(&ctx.repo),
                    pad: "digital".into(),
                    steps: Vec::new(),
                    max_frames: boot.frames,
                    min_distinct: cross.min_distinct,
                    idle_frames: boot.frames,
                },
                smoke: format!("boot-{}", b.name),
                tool_digest: tool_digest.clone(),
                disc_digest,
            });
        }
        for smoke in &cross.smoke {
            for pad in &cross.pads {
                let path = ctx.repo.join(smoke);
                let plan = match journey::plan(&ctx.repo, &path, pad, cross.min_distinct) {
                    Ok(p) => p,
                    Err(e) => {
                        checks.push(Check::fail(format!("{smoke} {pad}"), e));
                        continue;
                    }
                };
                let disc_digest = match util::sha256_file(&plan.disc.with_extension("bin")) {
                    Ok(d) => d,
                    Err(e) => {
                        checks.push(Check::fail(format!("{smoke} {pad}"), e));
                        continue;
                    }
                };
                let built = ctx
                    .artifacts
                    .values()
                    .any(|files| files.values().any(|d| *d == disc_digest));
                if !built {
                    checks.push(Check::fail(
                        format!("{smoke} {pad}"),
                        format!(
                            "{} is not a disc this run built: the smoke must boot a [[build]] \
                             artifact",
                            plan.disc.display()
                        ),
                    ));
                    continue;
                }
                jobs.push(Job {
                    emulator: *emulator,
                    plan,
                    smoke: smoke.clone(),
                    tool_digest: tool_digest.clone(),
                    disc_digest,
                });
            }
        }
    }
    // ReARMed jobs share the emulator budget; Redux runs one process at a time.
    let (redux, rearmed): (Vec<&Job>, Vec<&Job>) =
        jobs.iter().partition(|j| j.emulator == Emulator::Redux);
    let mut done = sched::run_all(&rearmed, ctx.config.budget.emulators, |j| {
        one(ctx, j, &self_exe)
    });
    done.extend(sched::run_all(
        &redux,
        ctx.config.budget.redux.min(ctx.config.budget.emulators),
        |j| one(ctx, j, &self_exe),
    ));
    let mut hits = 0;
    for (check, hit) in done {
        hits += usize::from(hit);
        checks.push(check);
    }
    let source = match hits {
        0 => Source::Run,
        n if n == jobs.len() => Source::Hit,
        _ => Source::Mixed,
    };
    result(
        source,
        Outcome {
            checks,
            ..Outcome::default()
        },
    )
}

fn one(ctx: &Ctx<'_>, job: &Job, self_exe: &Path) -> (Check, bool) {
    let stem = Path::new(&job.smoke)
        .file_stem()
        .map_or("smoke".into(), |s| s.to_string_lossy().into_owned());
    let name = format!("{stem} {} {}", job.emulator.name(), job.plan.pad);
    let mut key = Key::new("cross");
    key.part("emulator", job.emulator.name());
    key.part("tool", &job.tool_digest);
    key.part("disc", &job.disc_digest);
    key.part(
        "plan",
        &serde_json::to_string(&job.plan.steps).unwrap_or_default(),
    );
    key.part("pad", &job.plan.pad);
    key.part("min_distinct", &job.plan.min_distinct.to_string());
    let key = key.finish();
    if let Some(o) = ctx.receipts.get("cross", &key) {
        println!("RECEIPT HIT cross {name} {}", util::short(&key));
        let check = o
            .checks
            .into_iter()
            .next()
            .unwrap_or_else(|| Check::pass(&name, ""));
        return (check, true);
    }
    let out = ctx.work.join("cross").join(format!(
        "{stem}-{}-{}-{}",
        job.emulator.name(),
        job.plan.pad,
        util::short(&key)
    ));
    let replay = ctx.timed(&format!("cross:{name}"), || match job.emulator {
        Emulator::Rearmed => ctx.machine("emu", || rearmed(ctx, job, self_exe, &out)),
        Emulator::Redux => {
            let contents = ctx.config.tools.redux.clone().unwrap_or_default();
            let cfg = ctx.work.join("redux-cfg");
            let seconds = (job.plan.max_frames / 20).clamp(120, 1800);
            ctx.machine("redux", || {
                crate::redux::replay(&job.plan, &contents, &cfg, &out, seconds)
            })
        }
    });
    let check = judge(&name, &job.plan, replay, &out);
    let outcome = Outcome {
        checks: vec![check.clone()],
        ..Outcome::default()
    };
    let _ = ctx.receipts.put("cross", &key, &outcome);
    (check, false)
}

/// Run the ReARMed adapter in its own process (one core per process).
fn rearmed(ctx: &Ctx<'_>, job: &Job, self_exe: &Path, out: &Path) -> Result<Replay, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let plan_path = out.join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::to_string_pretty(&job.plan).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let core: PathBuf = ctx.config.tools.rearmed_core.clone().unwrap_or_default();
    let argv = vec![
        self_exe.display().to_string(),
        "adapter".into(),
        "rearmed".into(),
        plan_path.display().to_string(),
        core.display().to_string(),
        out.display().to_string(),
    ];
    let log = out.join("run.log");
    let seconds = (job.plan.max_frames / 20).clamp(120, 1800);
    let done = util::run_logged(&argv, out, &[], &log, Some(seconds))?;
    let text = std::fs::read_to_string(out.join("result.json")).map_err(|_| {
        format!(
            "the ReARMed adapter exited {}{} without a result; log {}",
            done.code,
            if done.timed_out { " (timeout)" } else { "" },
            log.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Turn a replay into the check.
pub fn judge(name: &str, plan: &Plan, replay: Result<Replay, String>, out: &Path) -> Check {
    let r = match replay {
        Ok(r) => r,
        Err(e) => return Check::fail(name, e),
    };
    let reached: Vec<String> = r.reached.iter().map(|(n, f)| format!("{n}@{f}")).collect();
    let value = format!(
        "{} frames, {} distinct, reached {}",
        r.frames,
        r.distinct,
        if reached.is_empty() {
            "nothing".into()
        } else {
            reached.join(" ")
        }
    );
    let wanted = plan
        .steps
        .iter()
        .filter(|s| matches!(s.action, journey::Action::Check { .. }))
        .count();
    if let Some(f) = r.failure {
        return Check::new(name, crate::report::Status::Fail, value)
            .detail(format!("{f}; evidence {}", out.display()));
    }
    if r.reached.len() != wanted {
        return Check::fail(
            name,
            format!(
                "{} of {wanted} checkpoints reached; evidence {}",
                r.reached.len(),
                out.display()
            ),
        );
    }
    if r.distinct < plan.min_distinct {
        return Check::fail(
            name,
            format!(
                "only {} distinct pictures (fewer than {}): a hang or a frozen picture; evidence {}",
                r.distinct,
                plan.min_distinct,
                out.display()
            ),
        );
    }
    Check::pass(name, value).detail(out.display().to_string())
}
