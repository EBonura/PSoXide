//! Host checks: commands the game declares (its own tests, format check, hazard scans) that must
//! exit 0 and leave the tree as they found it. Keyed by the command and the tree's bytes.

use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// The host stage.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let mut checks = Vec::new();
    let mut hits = 0;
    let files = util::tree_files(&ctx.repo).unwrap_or_default();
    for host in &ctx.game.hosts {
        let mut key = Key::new("host");
        key.part("command", &host.command.join("\0"));
        let mut command = host.command.clone();
        if let Some(name) = &host.build {
            let Some(b) = ctx.game.build(name) else {
                checks.push(Check::fail(&host.name, format!("unknown build `{name}`")));
                continue;
            };
            let out = b.out_dir(&ctx.repo).display().to_string();
            let cue = b.cue(&ctx.repo).display().to_string();
            for arg in &mut command {
                *arg = arg.replace("{out}", &out).replace("{disc}", &cue);
            }
            for (file, digest) in ctx.artifacts.get(name).into_iter().flatten() {
                key.part(file, digest);
            }
        }
        for f in &files {
            if let Err(e) = key.file(f, &ctx.repo.join(f)) {
                checks.push(Check::fail(&host.name, e));
            }
        }
        let key = key.finish();
        if let Some(outcome) = ctx.receipts.get("host", &key) {
            println!("RECEIPT HIT host {} {}", host.name, util::short(&key));
            hits += 1;
            checks.extend(outcome.checks);
            continue;
        }
        let outcome = ctx.timed(&format!("host:{}", host.name), || {
            let before = util::git(&ctx.repo, &["status", "--porcelain"]).unwrap_or_default();
            let log = match ctx.dir(&["host"]) {
                Ok(d) => d.join(format!("{}-{}.log", host.name, util::short(&key))),
                Err(e) => return Outcome::error(&host.name, e),
            };
            let stage_root = ctx.work.join("guest-stage");
            let done =
                util::run_logged(&command, &ctx.repo, &ctx.child_env(&stage_root), &log, None);
            let after = util::git(&ctx.repo, &["status", "--porcelain"]).unwrap_or_default();
            let check = match done {
                Ok(d) if d.code != 0 => Check::fail(
                    &host.name,
                    format!("exit {}; log {}", d.code, log.display()),
                ),
                Ok(_) if before != after => Check::fail(
                    &host.name,
                    format!(
                        "it changed the tree (stale committed files?): {}",
                        changed(&before, &after)
                    ),
                ),
                Ok(_) => Check::pass(&host.name, "exit 0"),
                Err(e) => Check::fail(&host.name, e),
            };
            Outcome {
                checks: vec![check],
                ..Outcome::default()
            }
        });
        let _ = ctx.receipts.put("host", &key, &outcome);
        checks.extend(outcome.checks);
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
        } else if hits == ctx.game.hosts.len() {
            Source::Hit
        } else {
            Source::Mixed
        },
        key: String::new(),
        outcome: Outcome {
            checks,
            ..Outcome::default()
        },
    }
}

fn changed(before: &str, after: &str) -> String {
    after
        .lines()
        .filter(|l| !before.lines().any(|b| b == *l))
        .map(|l| l.trim().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
