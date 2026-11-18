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
        // Original game files enter the key by content, never by path (originals.rs).
        let keyed: Result<Vec<String>, String> = build
            .env
            .values()
            .chain(build.commands.iter().flatten())
            .map(|v| ctx.key_text(v))
            .collect();
        let keyed = match keyed {
            Ok(k) => k,
            Err(e) => return failed(&build.name, e),
        };
        match ctx.build_originals(build) {
            Ok(originals) => {
                for (name, digest) in originals {
                    facts.insert(format!("input:{name}"), digest);
                }
            }
            Err(e) => return failed(&build.name, e),
        }
        let key = match key(&ctx.repo, build, &files, &keyed.join("\0")) {
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
            // A receipt from before the symbol list existed: derive it from the same map.
            if let Some(map) = build.map_path(&ctx.repo) {
                let list = out.join(SYMBOLS_FILE);
                if !list.is_file() {
                    if let Err(e) = write_symbols(&map, &list) {
                        return failed(&build.name, e);
                    }
                }
            }
            let mut outcome = stored.unwrap_or_default();
            // Judge the logs of the build the receipt stands for with today's rules (a
            // receipt written before a rule existed must not hide a refusal or a short bind).
            let logs = recorded_output(ctx, build, &key);
            if let Some(found) = refused(build, &logs) {
                outcome.checks.push(Check::fail(&build.name, found));
            }
            for c in bind_checks(build, &logs) {
                if !outcome.checks.iter().any(|o| o.name == c.name) {
                    outcome.checks.push(c);
                }
            }
            outcome
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
    let mut output = String::new();
    for (i, command) in build.commands.iter().enumerate() {
        let argv: Vec<String> = match command
            .iter()
            .map(|a| ctx.resolve(&a.replace("{out}", &out.display().to_string())))
            .collect::<Result<_, _>>()
        {
            Ok(a) => a,
            Err(e) => return Outcome::error(&build.name, e),
        };
        let log = log_dir.join(format!("{}-{}-{}.log", build.name, util::short(key), i + 1));
        match util::run_logged(&argv, &ctx.repo, &env, &log, None) {
            Ok(done) if done.code == 0 => {
                if let Some(found) = refused(build, &done.output) {
                    return Outcome::error(&build.name, format!("{found}; log {}", log.display()));
                }
                output.push_str(&done.output);
            }
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
    let last_path = log_dir.join(format!("last-{}.json", build.name));
    let last: BTreeMap<String, String> = std::fs::read_to_string(&last_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for (dest, source) in &build.collect {
        let found = collect(&ctx.repo, source, start).or_else(|stale| {
            // A build with nothing to relink leaves the old link in place: the same bytes the
            // last successful build of this name collected are still that link's file.
            let path = newest(&ctx.repo, source)?;
            let digest = util::sha256_file(&path)?;
            if last.get(dest) == Some(&digest) {
                Ok(path)
            } else {
                Err(stale)
            }
        });
        match found {
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
    if let Some(map) = build.map_path(&ctx.repo) {
        if let Err(e) = write_symbols(&map, &out.join(SYMBOLS_FILE)) {
            return Outcome::error(&build.name, e);
        }
    }
    let cue = build.cue(&ctx.repo);
    let bin = cue.with_extension("bin");
    let discs: &[&std::path::PathBuf] = if build.disc.is_empty() {
        &[]
    } else {
        &[&cue, &bin]
    };
    for f in discs {
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
    if let Ok(json) = serde_json::to_string(&files) {
        let _ = std::fs::write(&last_path, json);
    }
    let disc = files
        .get(&build.disc.replace(".cue", ".bin"))
        .map_or("?", |d| util::short(d))
        .to_string();
    let mut checks = vec![Check::pass(
        format!("{} built", build.name),
        format!("disc {disc}, {} files", files.len()),
    )];
    checks.extend(bind_checks(build, &output));
    Outcome {
        checks,
        facts: files,
    }
}

/// The command logs a build with this key wrote, concatenated (empty when they are gone).
fn recorded_output(ctx: &Ctx<'_>, build: &Build, key: &str) -> String {
    let dir = ctx.work.join("build");
    let mut text = String::new();
    for i in 1..=build.commands.len() {
        let log = dir.join(format!("{}-{}-{i}.log", build.name, util::short(key)));
        text.push_str(&std::fs::read_to_string(log).unwrap_or_default());
    }
    text
}

/// The symbol list every build with a map writes into its `out`: `address name` per symbol,
/// with lld's `(.N)` section suffix stripped, which the gate and the harness both read.
pub const SYMBOLS_FILE: &str = "symbols.txt";

/// psoxide-pgo's own refusal lines: a driver that catches them and links anyway still fails.
const PGO_REFUSALS: &[&str] = &["UNBOUND PROFILE:", "the layout profile binds"];

/// The first `fail_on` substring or psoxide-pgo refusal in a build command's output.
pub fn refused(build: &Build, output: &str) -> Option<String> {
    for needle in build
        .fail_on
        .iter()
        .map(String::as_str)
        .chain(PGO_REFUSALS.iter().copied())
    {
        if let Some(line) = output.lines().find(|l| l.contains(needle)) {
            return Some(format!("the build printed `{}`", line.trim()));
        }
    }
    None
}

/// One named check per psoxide-pgo bind line in the build's output (placement layouts and
/// sample profiles): the bound share against `bind_min` (default 100%).
pub fn bind_checks(build: &Build, output: &str) -> Vec<Check> {
    let min = build.bind_min.unwrap_or(100.0);
    let mut checks = Vec::new();
    for line in output.lines() {
        let (what, rest) = if let Some(rest) = line.strip_prefix("psoxide-pgo: layout ") {
            ("placement layout", rest)
        } else if let Some(at) = line.find("profile bind (") {
            ("sample profile", &line[at..])
        } else {
            continue;
        };
        let Some(percent) = first_percent(rest) else {
            continue;
        };
        let name = format!("{} {what} bind", build.name);
        let value = format!("{percent:.1}%");
        let check = if percent + 1e-9 >= min {
            Check::pass(name, value)
        } else {
            Check::new(name, crate::report::Status::Fail, value).detail(format!(
                "under {min}%: collect the layout or profile again on this build, or set \
                 bind_min with bind_reason; {}",
                line.trim()
            ))
        };
        checks.push(match &build.bind_reason {
            Some(r) if min < 100.0 => check.baseline(format!("bind_min {min}%: {r}")),
            _ => check,
        });
    }
    checks
}

/// The first `NN.N%` in `text`.
fn first_percent(text: &str) -> Option<f64> {
    let at = text.find('%')?;
    let head = &text[..at];
    let start = head
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or(0, |i| i + 1);
    head[start..].parse().ok()
}

/// Write `out` from the map at `map`: every symbol as `address name`, `(.N)` stripped.
pub fn write_symbols(map: &Path, out: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(map).map_err(|e| format!("read {}: {e}", map.display()))?;
    let parsed = psoxide_size::map::parse(&text);
    let mut lines = std::collections::BTreeSet::new();
    for section in &parsed.sections {
        for sym in &section.syms {
            lines.insert(format!(
                "{:08x} {}",
                sym.vma,
                strip_section_suffix(&sym.name)
            ));
        }
    }
    let mut body: String = lines.into_iter().map(|l| l + "\n").collect();
    if body.is_empty() {
        body.push_str("# no symbols in the map\n");
    }
    std::fs::write(out, body).map_err(|e| format!("write {}: {e}", out.display()))
}

/// `name (.0)` -> `name`: lld's suffix for a symbol in a numbered section.
pub fn strip_section_suffix(name: &str) -> &str {
    match name.rsplit_once(" (.") {
        Some((head, tail)) if tail.ends_with(')') => head,
        _ => name,
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

/// The file a `collect` source names now, whatever its age: the path itself, or the newest
/// match of a trailing `*`.
fn newest(repo: &Path, source: &str) -> Result<PathBuf, String> {
    let Some(prefix) = source.strip_suffix('*') else {
        let path = repo.join(source);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("{source} does not exist"))
        };
    };
    util::glob(repo, &format!("{prefix}*"))?
        .into_iter()
        .filter_map(|p| util::mtime_seconds(&p).map(|t| (t, p)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
        .ok_or_else(|| format!("nothing matches {source}"))
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

    const LAYOUT_LINE: &str = "psoxide-pgo: layout /x/pgo/game.layout: bound 136 of 138 \
                               functions, 99.9% of the profile's instructions; 2 changed, 0 missing";

    #[test]
    fn a_bind_under_100_is_red_unless_declared() {
        let b = build(&[]);
        let c = bind_checks(&b, LAYOUT_LINE);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].status, crate::report::Status::Fail);
        assert_eq!(c[0].value, "99.9%");
        let mut b = build(&[]);
        b.bind_min = Some(99.5);
        b.bind_reason = Some("two functions changed since the layout".into());
        assert_eq!(
            bind_checks(&b, LAYOUT_LINE)[0].status,
            crate::report::Status::Pass
        );
        assert!(bind_checks(&b, "no pgo here").is_empty());
        let profile = "psoxide-pgo: profile bind (names normalised): 98.6% of samples bound; x";
        assert_eq!(bind_checks(&b, profile)[0].value, "98.6%");
    }

    #[test]
    fn a_printed_refusal_fails_the_build() {
        let mut b = build(&[]);
        assert!(refused(&b, "all fine").is_none());
        assert!(refused(&b, "UNBOUND PROFILE: only 40.0% ...").is_some());
        b.fail_on = vec!["built WITHOUT the PGO link order".into()];
        assert!(refused(&b, "WARNING: built WITHOUT the PGO link order")
            .unwrap()
            .contains("WITHOUT"));
    }

    #[test]
    fn section_suffixes_are_stripped() {
        assert_eq!(
            strip_section_suffix("cs_psx::SIM_NOW (.0)"),
            "cs_psx::SIM_NOW"
        );
        assert_eq!(strip_section_suffix("plain"), "plain");
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
