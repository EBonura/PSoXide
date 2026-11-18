//! Install: through `psx-library` (the one install implementation: temporary name, digest check,
//! rename, cue last), and only bytes that a complete landing verdict judged. The provenance adds
//! what only the harness knows: the SDK pin, the gate revision, the harness version, the result
//! file's hash and the digest of every installed file; the link map goes beside the disc named
//! after the SDK pin.

use std::path::Path;

use crate::config::GATE_REVISION;
use crate::report::{Check, Outcome, RunResult, Source, StageResult, Status};
use crate::stage::{sdk_pin, Ctx};
use crate::util;

/// The install stage: reads the latest result in the work tree and refuses unless it is a
/// complete PASS for exactly these bytes.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let mut checks = Vec::new();
    for install in &ctx.game.installs {
        checks.extend(one(ctx, install).outcome.checks);
    }
    if ctx.game.installs.is_empty() {
        checks.push(Check::skip("install", "the game file has no [[install]]"));
    }
    StageResult {
        name: "install".into(),
        source: Source::Run,
        key: String::new(),
        outcome: Outcome {
            checks,
            ..Outcome::default()
        },
    }
}

fn one(ctx: &Ctx<'_>, install: &crate::game::Install) -> StageResult {
    let result = |outcome| StageResult {
        name: "install".into(),
        source: Source::Run,
        key: String::new(),
        outcome,
    };
    let Some(library) = &ctx.config.library else {
        return result(Outcome::error(
            "install",
            "no library in the psoxide-test config: set `library`",
        ));
    };
    let Some(b) = ctx.game.build(&install.build) else {
        return result(Outcome::error("install", "unknown build"));
    };
    let results = ctx.work.join("results");
    let verdict = match latest(&results) {
        Ok(v) => v,
        Err(e) => return result(Outcome::error("install", e)),
    };
    let (hash, judged) = verdict;
    if judged.verdict != Status::Pass || !judged.landing_missing.is_empty() {
        return result(Outcome::error(
            "install",
            format!(
                "the latest result ({}) is not a complete landing PASS: run `psoxide-test run \
                 --tier full` first",
                util::short(&hash)
            ),
        ));
    }
    let now = ctx.artifacts.get(&b.name).cloned().unwrap_or_default();
    let judged_files: std::collections::BTreeMap<String, String> = judged
        .stages
        .iter()
        .filter(|s| s.name == "build")
        .flat_map(|s| s.outcome.facts.iter())
        .filter_map(|(k, v)| {
            k.strip_prefix(&format!("{}/", b.name))
                .map(|f| (f.to_string(), v.clone()))
        })
        .collect();
    if now.is_empty() || now != judged_files {
        return result(Outcome::error(
            "install",
            format!(
                "the files in {} are not the bytes result {} judged: build and judge them again",
                b.out,
                util::short(&hash)
            ),
        ));
    }
    let name = install
        .name
        .clone()
        .unwrap_or_else(|| ctx.game.name.clone());
    let games_dir = library.join(&name);
    let pin = sdk_pin(&ctx.repo);
    let base = serde_json::json!({
        "harness": { "tool": "psoxide-test", "version": env!("CARGO_PKG_VERSION"),
                     "result_sha256": hash },
        "sdk_pin": pin,
        "emulator_gate_revision": GATE_REVISION,
        "files": now,
    });
    let source = match psx_library::Source::from_git(&ctx.repo, &ctx.game.repository) {
        Ok(s) => s,
        Err(e) => return result(Outcome::error("install", e.to_string())),
    };
    let map_bytes = b
        .map_path(&ctx.repo)
        .and_then(|m| std::fs::read(m).ok())
        .unwrap_or_default();
    let suffix = format!(".{}.map", util::short(&pin));
    let sidecars = [psx_library::Sidecar::new(&suffix, &map_bytes)];
    let cue = b.cue(&ctx.repo);
    let request =
        psx_library::Request::new(&cue, &games_dir, &name, source).with_provenance_base(&base);
    let request = if map_bytes.is_empty() {
        request
    } else {
        request.with_sidecars(&sidecars)
    };
    match ctx.timed("install", || psx_library::install(&request)) {
        Ok(done) => result(Outcome {
            checks: vec![Check::pass(
                "installed",
                format!(
                    "{} sha256 {}",
                    done.bin.display(),
                    util::short(&done.bin_sha256)
                ),
            )
            .detail(done.provenance.display().to_string())],
            ..Outcome::default()
        }),
        Err(e) => result(Outcome::error("install", e.to_string())),
    }
}

/// The newest result in `dir`: its hash and contents.
pub fn latest(dir: &Path) -> Result<(String, RunResult), String> {
    let hash = std::fs::read_to_string(dir.join("latest"))
        .map_err(|_| format!("no result in {}: run the harness first", dir.display()))?;
    let hash = hash.trim().to_string();
    let text = std::fs::read_to_string(dir.join(format!("{hash}.json")))
        .map_err(|e| format!("read result {hash}: {e}"))?;
    if util::sha256_bytes(text.as_bytes()) != hash {
        return Err(format!(
            "result {hash} does not match its own hash: it was edited"
        ));
    }
    let r: RunResult = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok((hash, r))
}
