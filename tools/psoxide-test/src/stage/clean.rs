//! Clean-clone proof: a fresh clone of the pushed commit, plus only the declared self-verifying
//! caches (`seed`, APFS-cloned in), rebuilds the declared build (after its `before` builds) and
//! every compared file is byte-identical to the working build's. One proof per `[[clean]]`. The
//! clone is deleted afterwards; its logs stay.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::game::{Build, Clean};
use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// The clean stage: every `[[clean]]` in order.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    if ctx.game.cleans.is_empty() {
        return StageResult {
            name: "clean".into(),
            source: Source::Run,
            key: String::new(),
            outcome: Outcome {
                checks: vec![Check::skip("clean", "the game file has no [[clean]]")],
                ..Outcome::default()
            },
        };
    }
    let mut checks = Vec::new();
    let mut keys = Vec::new();
    let mut hits = 0;
    let several = ctx.game.cleans.len() > 1;
    for clean in &ctx.game.cleans {
        let (source, key, outcome) = one(ctx, clean);
        hits += usize::from(source == Source::Hit);
        keys.push(util::short(&key).to_string());
        for mut c in outcome.checks {
            if several && !c.name.starts_with(&format!("{} ", clean.build)) {
                c.name = format!("{} {}", clean.build, c.name);
            }
            checks.push(c);
        }
    }
    StageResult {
        name: "clean".into(),
        source: match hits {
            0 => Source::Run,
            n if n == ctx.game.cleans.len() => Source::Hit,
            _ => Source::Mixed,
        },
        key: keys.join(","),
        outcome: Outcome {
            checks,
            ..Outcome::default()
        },
    }
}

fn one(ctx: &Ctx<'_>, clean: &Clean) -> (Source, String, Outcome) {
    let error = |e: String| (Source::Run, String::new(), Outcome::error("clean", e));
    let Some(b) = ctx.game.build(&clean.build) else {
        return error("unknown build".into());
    };
    let commit = match util::git(&ctx.repo, &["rev-parse", "HEAD"]) {
        Ok(c) => c,
        Err(e) => return error(e),
    };
    let dirty = util::git(
        &ctx.repo,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .unwrap_or_default();
    if !dirty.is_empty() {
        return error(
            "tracked files differ from HEAD: the clean clone proves a commit; commit and push first"
                .into(),
        );
    }
    let compare: Vec<String> = if clean.compare.is_empty() {
        vec![b.disc.clone(), b.disc.replace(".cue", ".bin")]
    } else {
        clean.compare.clone()
    };
    let Some(expected) = ctx.artifacts.get(&b.name) else {
        return error("the build stage did not run in this invocation".into());
    };
    let mut key = Key::new("clean");
    key.part("commit", &commit);
    key.part("build", &b.name);
    key.part("before", &clean.before.join("\0"));
    key.part("history", &clean.history.to_string());
    key.part("seed", &clean.seed.join("\0"));
    // The clone builds from the same original game files: keyed by their content.
    for name in clean.before.iter().chain([&clean.build]) {
        let Some(build) = ctx.game.build(name) else {
            return error(format!("unknown build `{name}`"));
        };
        match ctx.build_originals(build) {
            Ok(originals) => {
                for (input, digest) in originals {
                    key.part(&format!("input:{input}"), &digest);
                }
            }
            Err(e) => return error(e),
        }
    }
    for f in &compare {
        key.part(f, expected.get(f).map_or("", String::as_str));
    }
    let key = key.finish();
    if let Some(o) = ctx.receipts.get("clean", &key) {
        println!("RECEIPT HIT clean {} {}", b.name, util::short(&key));
        return (Source::Hit, key, o);
    }
    let outcome = ctx.timed(&format!("clean:{}", b.name), || {
        prove(ctx, clean, b, &commit, &compare, expected)
    });
    let _ = ctx.receipts.put("clean", &key, &outcome);
    (Source::Run, key, outcome)
}

fn fetch_steps<'a>(url: &'a str, commit: &'a str, history: bool) -> Vec<Vec<&'a str>> {
    let mut fetch = vec!["fetch", "-q"];
    if !history {
        fetch.extend(["--depth", "1"]);
    }
    fetch.extend(["origin", commit]);
    vec![
        vec!["init", "-q"],
        vec!["remote", "add", "origin", url],
        fetch,
        vec!["checkout", "-q", "FETCH_HEAD"],
    ]
}

fn prove(
    ctx: &Ctx<'_>,
    clean: &Clean,
    b: &Build,
    commit: &str,
    compare: &[String],
    expected: &BTreeMap<String, String>,
) -> Outcome {
    let root = ctx
        .work
        .join("clean")
        .join(format!("{}-{}", util::short(commit), b.name));
    if root.exists() {
        if let Err(e) = std::fs::remove_dir_all(&root) {
            return Outcome::error("clean", format!("remove old {}: {e}", root.display()));
        }
    }
    let src = root.join("src");
    let logs = root.join("logs");
    let url = format!("https://github.com/{}.git", ctx.game.repository);
    if let Err(e) = std::fs::create_dir_all(&src) {
        return Outcome::error("clean", e.to_string());
    }
    // A private repository fetches through the configured helper; the helper runs at fetch
    // time, so no token is ever in an argument, the clone's config or a log.
    let helper: Vec<String> = match &ctx.config.tools.git_credential_helper {
        Some(h) => vec![
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            format!("credential.helper={h}"),
        ],
        None => Vec::new(),
    };
    for args in fetch_steps(&url, commit, clean.history) {
        let mut all: Vec<&str> = helper.iter().map(String::as_str).collect();
        all.extend(args);
        if let Err(e) = util::git(&src, &all) {
            return Outcome::error(
                "clean",
                format!(
                    "{e}\n  is {commit} pushed to {}? push the branch first (a private repository \
                     needs [tools] git_credential_helper in the config)",
                    ctx.game.repository
                ),
            );
        }
    }
    for s in &clean.seed {
        let from = ctx.repo.join(s);
        if !from.exists() {
            return Outcome::error("clean", format!("seed {} does not exist", from.display()));
        }
        let to = src.join(s);
        let status = Command::new("cp").arg("-cR").arg(&from).arg(&to).status();
        if !status.is_ok_and(|s| s.success()) {
            return Outcome::error("clean", format!("APFS clone of {} failed", from.display()));
        }
        // A seed carries sources and verified caches, never build output: a host binary bakes
        // the path it was built at, and run from the clone it would read the working tree.
        let built = to.join("target");
        if built.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&built) {
                return Outcome::error("clean", format!("remove {}: {e}", built.display()));
            }
        }
    }
    for name in &clean.before {
        let Some(before) = ctx.game.build(name) else {
            return Outcome::error("clean", format!("unknown build `{name}`"));
        };
        if let Err(e) = build_in_clone(ctx, before, &src, &root, &logs) {
            return Outcome::error("clean", e);
        }
    }
    if let Err(e) = build_in_clone(ctx, b, &src, &root, &logs) {
        return Outcome::error("clean", e);
    }
    let out = b.out_dir(&src);
    let mut checks = Vec::new();
    for f in compare {
        let got = util::sha256_file(&out.join(f)).unwrap_or_else(|e| e);
        let want = expected.get(f).cloned().unwrap_or_default();
        checks.push(if got == want {
            Check::pass(format!("clean {f}"), util::short(&got).to_string())
        } else {
            Check::fail(
                format!("clean {f}"),
                format!(
                    "clone {} != working build {}",
                    util::short(&got),
                    util::short(&want)
                ),
            )
        });
    }
    let kept = logs_into(ctx, &logs, commit);
    if let Err(e) = std::fs::remove_dir_all(&root) {
        checks.push(Check::fail(
            "clean cleanup",
            format!("remove {}: {e}", root.display()),
        ));
    }
    if let Some(dir) = kept {
        if let Some(c) = checks.first_mut() {
            c.detail = format!("logs {}", dir.display());
        }
    }
    Outcome {
        checks,
        ..Outcome::default()
    }
}

/// Run one build's commands in the clone and collect its files into its `out` there.
fn build_in_clone(
    ctx: &Ctx<'_>,
    b: &Build,
    src: &Path,
    root: &Path,
    logs: &Path,
) -> Result<(), String> {
    let out = b.out_dir(src);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let env = ctx.build_env(b, &root.join("guest-stage"))?;
    let start = crate::util::now_seconds() - 1.0;
    for (i, command) in b.commands.iter().enumerate() {
        let argv: Vec<String> = command
            .iter()
            .map(|a| ctx.resolve(&a.replace("{out}", &out.display().to_string())))
            .collect::<Result<_, _>>()?;
        let log = logs.join(format!("{}-{}.log", b.name, i + 1));
        let d = util::run_logged(&argv, src, &env, &log, None)?;
        if d.code != 0 {
            return Err(format!(
                "`{}` exited {} in the clone; log {}",
                argv.join(" "),
                d.code,
                log.display()
            ));
        }
        if let Some(found) = super::build::refused(b, &d.output) {
            return Err(format!("{found} in the clone; log {}", log.display()));
        }
    }
    for (dest, source) in &b.collect {
        let path = super::build::collect(src, source, start)?;
        std::fs::copy(&path, out.join(dest))
            .map_err(|e| format!("copy {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Keep the clone's build logs after the clone is deleted.
fn logs_into(ctx: &Ctx<'_>, logs: &Path, commit: &str) -> Option<std::path::PathBuf> {
    let keep = ctx.dir(&["clean-logs", util::short(commit)]).ok()?;
    for entry in std::fs::read_dir(logs).ok()?.flatten() {
        let _ = std::fs::copy(entry.path(), keep.join(entry.file_name()));
    }
    Some(keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_history_clone_fetches_without_depth() {
        let shallow = fetch_steps("u", "c", false);
        assert!(shallow[2].contains(&"--depth"));
        let full = fetch_steps("u", "c", true);
        assert!(!full[2].contains(&"--depth"));
    }
}
