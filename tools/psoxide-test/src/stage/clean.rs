//! Clean-clone proof: a fresh clone of the pushed commit, plus only the declared self-verifying
//! caches (`seed`, APFS-cloned in), rebuilds the declared build and every compared file is
//! byte-identical to the working build's. The clone is deleted afterwards; its log stays.

use std::path::Path;
use std::process::Command;

use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// The clean stage.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let result = |source, key: String, outcome| StageResult {
        name: "clean".into(),
        source,
        key,
        outcome,
    };
    let Some(clean) = &ctx.game.clean else {
        return result(
            Source::Run,
            String::new(),
            Outcome {
                checks: vec![Check::skip("clean", "the game file has no [clean]")],
                ..Outcome::default()
            },
        );
    };
    let Some(b) = ctx.game.build(&clean.build) else {
        return result(
            Source::Run,
            String::new(),
            Outcome::error("clean", "unknown build"),
        );
    };
    let commit = match util::git(&ctx.repo, &["rev-parse", "HEAD"]) {
        Ok(c) => c,
        Err(e) => return result(Source::Run, String::new(), Outcome::error("clean", e)),
    };
    let dirty = util::git(
        &ctx.repo,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .unwrap_or_default();
    if !dirty.is_empty() {
        return result(
            Source::Run,
            String::new(),
            Outcome::error(
                "clean",
                "tracked files differ from HEAD: the clean clone proves a commit; commit and push first",
            ),
        );
    }
    let compare: Vec<String> = if clean.compare.is_empty() {
        vec![b.disc.clone(), b.disc.replace(".cue", ".bin")]
    } else {
        clean.compare.clone()
    };
    let Some(expected) = ctx.artifacts.get(&b.name) else {
        return result(
            Source::Run,
            String::new(),
            Outcome::error("clean", "the build stage did not run in this invocation"),
        );
    };
    let mut key = Key::new("clean");
    key.part("commit", &commit);
    key.part("seed", &clean.seed.join("\0"));
    // The clone builds from the same original game files: keyed by their content.
    match ctx.build_originals(b) {
        Ok(originals) => {
            for (name, digest) in originals {
                key.part(&format!("input:{name}"), &digest);
            }
        }
        Err(e) => return result(Source::Run, String::new(), Outcome::error("clean", e)),
    }
    for f in &compare {
        key.part(f, expected.get(f).map_or("", String::as_str));
    }
    let key = key.finish();
    if let Some(o) = ctx.receipts.get("clean", &key) {
        println!("RECEIPT HIT clean {}", util::short(&key));
        return result(Source::Hit, key, o);
    }
    let outcome = ctx.timed("clean", || {
        prove(ctx, b, &commit, &clean.seed, &compare, expected)
    });
    let _ = ctx.receipts.put("clean", &key, &outcome);
    result(Source::Run, key, outcome)
}

fn prove(
    ctx: &Ctx<'_>,
    b: &crate::game::Build,
    commit: &str,
    seed: &[String],
    compare: &[String],
    expected: &std::collections::BTreeMap<String, String>,
) -> Outcome {
    let root = ctx.work.join("clean").join(util::short(commit));
    if root.exists() {
        if let Err(e) = std::fs::remove_dir_all(&root) {
            return Outcome::error("clean", format!("remove old {}: {e}", root.display()));
        }
    }
    let src = root.join("src");
    let logs = root.join("logs");
    let url = format!("https://github.com/{}.git", ctx.game.repository);
    let git_steps: [&[&str]; 4] = [
        &["init", "-q"],
        &["remote", "add", "origin", &url],
        &["fetch", "-q", "--depth", "1", "origin", commit],
        &["checkout", "-q", "FETCH_HEAD"],
    ];
    if let Err(e) = std::fs::create_dir_all(&src) {
        return Outcome::error("clean", e.to_string());
    }
    for args in git_steps {
        if let Err(e) = util::git(&src, args) {
            return Outcome::error(
                "clean",
                format!(
                    "{e}\n  is {commit} pushed to {}? push the branch first",
                    ctx.game.repository
                ),
            );
        }
    }
    for s in seed {
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
    let out = b.out_dir(&src);
    if let Err(e) = std::fs::create_dir_all(&out) {
        return Outcome::error("clean", e.to_string());
    }
    let env = match ctx.build_env(b, &root.join("guest-stage")) {
        Ok(e) => e,
        Err(e) => return Outcome::error("clean", e),
    };
    let start = crate::util::now_seconds() - 1.0;
    for (i, command) in b.commands.iter().enumerate() {
        let argv: Vec<String> = match command
            .iter()
            .map(|a| ctx.resolve(&a.replace("{out}", &out.display().to_string())))
            .collect::<Result<_, _>>()
        {
            Ok(a) => a,
            Err(e) => return Outcome::error("clean", e),
        };
        let log = logs.join(format!("{}-{}.log", b.name, i + 1));
        match util::run_logged(&argv, &src, &env, &log, None) {
            Ok(d) if d.code == 0 => {}
            Ok(d) => {
                return Outcome::error(
                    "clean",
                    format!(
                        "`{}` exited {} in the clone; log {}",
                        argv.join(" "),
                        d.code,
                        log.display()
                    ),
                )
            }
            Err(e) => return Outcome::error("clean", e),
        }
    }
    for (dest, source) in &b.collect {
        match super::build::collect(&src, source, start) {
            Ok(path) => {
                if let Err(e) = std::fs::copy(&path, out.join(dest)) {
                    return Outcome::error("clean", format!("copy {}: {e}", path.display()));
                }
            }
            Err(e) => return Outcome::error("clean", e),
        }
    }
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

/// Keep the clone's build logs after the clone is deleted.
fn logs_into(ctx: &Ctx<'_>, logs: &Path, commit: &str) -> Option<std::path::PathBuf> {
    let keep = ctx.dir(&["clean-logs", util::short(commit)]).ok()?;
    for entry in std::fs::read_dir(logs).ok()?.flatten() {
        let _ = std::fs::copy(entry.path(), keep.join(entry.file_name()));
    }
    Some(keep)
}
