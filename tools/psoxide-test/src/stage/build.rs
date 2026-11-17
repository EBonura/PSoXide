//! Build: run each declared build's commands, collect its artifacts into `out`, record the digest
//! of every file there and the SDK pin. Keyed by the bytes of the build's inputs, so an unchanged
//! tree never rebuilds and a cook that lists only its sources never re-cooks for a code change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::game::Build;
use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::{sdk_pin, Ctx};
use crate::util::{self, Key};

/// The receipt key of one build: its declaration and the bytes of its inputs.
pub fn key(repo: &Path, build: &Build, files: &[String], inputs: &str) -> Result<String, String> {
    let mut key = Key::new("build");
    key.part("inputs", inputs);
    key.part(
        "declaration",
        &serde_json::to_string(build).map_err(|e| e.to_string())?,
    );
    for f in files {
        let selected = if build.inputs.is_empty() {
            !build.exclude.iter().any(|x| f.starts_with(x.as_str()))
        } else {
            build.inputs.iter().any(|i| f.starts_with(i.as_str()))
        };
        if selected {
            key.file(f, &repo.join(f))?;
        }
    }
    Ok(key.finish())
}

/// Every file directly in `out` and its digest.
pub fn digests(out: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut files = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(out) else {
        return Ok(files);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            files.insert(
                entry.file_name().to_string_lossy().into_owned(),
                util::sha256_file(&path)?,
            );
        }
    }
    Ok(files)
}

/// Run the build stage for every declared build, one at a time.
pub fn run(ctx: &mut Ctx<'_>) -> StageResult {
    let files = match util::tree_files(&ctx.repo) {
        Ok(f) => f,
        Err(e) => return failed("tree", e),
    };
    let mut checks = Vec::new();
    let mut facts = BTreeMap::new();
    let mut hits = 0;
    let mut keys = Vec::new();
    facts.insert("sdk_pin".into(), sdk_pin(&ctx.repo));
    for build in &ctx.game.builds {
        let resolved: Vec<String> = build
            .env
            .values()
            .map(|v| super::resolve_inputs(v, &ctx.config.inputs).unwrap_or_default())
            .collect();
        let key = match key(&ctx.repo, build, &files, &resolved.join("\0")) {
            Ok(k) => k,
            Err(e) => return failed(&build.name, e),
        };
        keys.push(util::short(&key).to_string());
        let out = build.out_dir(&ctx.repo);
        let stored = ctx.receipts.get("build", &key);
        let present = digests(&out).unwrap_or_default();
        let reused = stored.as_ref().is_some_and(|o| {
            o.facts
                .iter()
                .all(|(name, digest)| present.get(name) == Some(digest))
        });
        let outcome = if reused {
            hits += 1;
            println!("RECEIPT HIT build {} {}", build.name, util::short(&key));
            stored.unwrap_or_default()
        } else {
            let outcome = ctx.timed(&format!("build:{}", build.name), || one(ctx, build, &key));
            if let Err(e) = ctx.receipts.put("build", &key, &outcome) {
                return failed(&build.name, e);
            }
            outcome
        };
        let ok = outcome.passed();
        ctx.artifacts
            .insert(build.name.clone(), outcome.facts.clone());
        for (name, digest) in &outcome.facts {
            facts.insert(format!("{}/{name}", build.name), digest.clone());
        }
        checks.extend(outcome.checks);
        if !ok {
            break;
        }
    }
    let source = match hits {
        0 => Source::Run,
        n if n == ctx.game.builds.len() => Source::Hit,
        _ => Source::Mixed,
    };
    StageResult {
        name: "build".into(),
        source,
        key: keys.join(","),
        outcome: Outcome { checks, facts },
    }
}

fn failed(what: &str, message: String) -> StageResult {
    StageResult {
        name: "build".into(),
        source: Source::Run,
        key: String::new(),
        outcome: Outcome::error(what, message),
    }
}

/// Run one build now.
fn one(ctx: &Ctx<'_>, build: &Build, key: &str) -> Outcome {
    let out = build.out_dir(&ctx.repo);
    if let Err(e) = std::fs::create_dir_all(&out) {
        return Outcome::error(&build.name, format!("create {}: {e}", out.display()));
    }
    let log_dir = match ctx.dir(&["build"]) {
        Ok(d) => d,
        Err(e) => return Outcome::error(&build.name, e),
    };
    let stage_root = match ctx.dir(&["guest-stage"]) {
        Ok(d) => d,
        Err(e) => return Outcome::error(&build.name, e),
    };
    let env = match ctx.build_env(build, &stage_root) {
        Ok(e) => e,
        Err(e) => return Outcome::error(&build.name, e),
    };
    let start = util::now_seconds() - 1.0;
    for (i, command) in build.commands.iter().enumerate() {
        let argv: Vec<String> = command
            .iter()
            .map(|a| a.replace("{out}", &out.display().to_string()))
            .collect();
        let log = log_dir.join(format!("{}-{}-{}.log", build.name, util::short(key), i + 1));
        match util::run_logged(&argv, &ctx.repo, &env, &log, None) {
            Ok(done) if done.code == 0 => {}
            Ok(done) => {
                return Outcome::error(
                    &build.name,
                    format!(
                        "`{}` exited {}; log {}",
                        argv.join(" "),
                        done.code,
                        log.display()
                    ),
                )
            }
            Err(e) => return Outcome::error(&build.name, e),
        }
    }
    for (dest, source) in &build.collect {
        match collect(&ctx.repo, source, start) {
            Ok(path) => {
                if let Err(e) = std::fs::copy(&path, out.join(dest)) {
                    return Outcome::error(
                        &build.name,
                        format!("copy {} into {}: {e}", path.display(), out.display()),
                    );
                }
            }
            Err(e) => return Outcome::error(&build.name, e),
        }
    }
    let cue = build.cue(&ctx.repo);
    let bin = cue.with_extension("bin");
    for f in [&cue, &bin] {
        if util::mtime_seconds(f).is_none_or(|t| t < start) {
            return Outcome::error(
                &build.name,
                format!(
                    "the build did not write {}: its commands must produce the disc in out",
                    f.display()
                ),
            );
        }
    }
    let files = match digests(&out) {
        Ok(f) => f,
        Err(e) => return Outcome::error(&build.name, e),
    };
    let disc = files
        .get(&build.disc.replace(".cue", ".bin"))
        .map_or("?", |d| util::short(d))
        .to_string();
    Outcome {
        checks: vec![Check::pass(
            format!("{} built", build.name),
            format!("disc {disc}, {} files", files.len()),
        )],
        facts: files,
    }
}

/// The file a `collect` source names. A trailing `*` takes the newest file with that prefix,
/// which must have been written by this build (so a stale map is never taken for a new link).
pub fn collect(repo: &Path, source: &str, since: f64) -> Result<PathBuf, String> {
    let Some(prefix) = source.strip_suffix('*') else {
        let path = repo.join(source);
        return match util::mtime_seconds(&path) {
            Some(t) if t >= since => Ok(path),
            Some(_) => Err(format!(
                "{source} was not written by this build (older than its start)"
            )),
            None => Err(format!("{source} does not exist after the build")),
        };
    };
    let matches = util::glob(repo, &format!("{prefix}*"))?;
    let newest = matches
        .into_iter()
        .filter_map(|p| util::mtime_seconds(&p).map(|t| (t, p)))
        .max_by(|a, b| a.0.total_cmp(&b.0));
    match newest {
        Some((t, p)) if t >= since => Ok(p),
        Some((_, p)) => Err(format!(
            "no file matching {source} was written by this build (newest is {}): the link did \
             not run; remove it and build again",
            p.display()
        )),
        None => Err(format!("nothing matches {source} after the build")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(inputs: &[&str]) -> Build {
        crate::game::Game::parse(&format!(
            "schema = 1\nname = \"g\"\nrepository = \"o/g\"\n[[build]]\nname = \"b\"\n\
             out = \"o\"\ncommands = [[\"true\"]]\ndisc = \"g.cue\"\ninputs = {inputs:?}\n"
        ))
        .unwrap()
        .builds
        .remove(0)
    }

    #[test]
    fn the_key_follows_only_the_declared_inputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("assets/a.png"), "a").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "1").unwrap();
        let files = vec!["assets/a.png".to_string(), "src/main.rs".to_string()];
        let cook = build(&["assets/"]);
        let code = build(&[]);
        let (k_cook, k_code) = (
            key(dir.path(), &cook, &files, "").unwrap(),
            key(dir.path(), &code, &files, "").unwrap(),
        );
        std::fs::write(dir.path().join("src/main.rs"), "2").unwrap();
        assert_eq!(key(dir.path(), &cook, &files, "").unwrap(), k_cook);
        assert_ne!(key(dir.path(), &code, &files, "").unwrap(), k_code);
    }

    #[test]
    fn a_collect_glob_refuses_a_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("maps")).unwrap();
        std::fs::write(dir.path().join("maps/a.map"), "").unwrap();
        let later = util::now_seconds() + 60.0;
        assert!(collect(dir.path(), "maps/*", later)
            .unwrap_err()
            .contains("not run"));
        assert!(collect(dir.path(), "maps/*", 0.0).is_ok());
    }
}
