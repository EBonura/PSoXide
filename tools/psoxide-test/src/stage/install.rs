//! Install: through `psx-library` (the one install implementation: temporary name, digest check,
//! rename, cue last), and only bytes that a complete landing verdict judged. The provenance adds
//! what only the harness knows: the SDK pin, the gate revision, the harness version, the result
//! file's hash and the digest of every installed file; the link map goes beside the disc named
//! after the SDK pin.
//!
//! The install guarantee lives in [`check`], which both this stage and `psoxide-test
//! install-check` (the campaign's install script) call: the result must be a complete, clean,
//! full PASS on the install profile, and no speed window may be slower than the build installed
//! before it, whose speed facts sit beside it as `<Name>.speed-facts.json` ([`SpeedFacts`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::baseline::WindowBase;
use crate::config::GATE_REVISION;
use crate::report::{Check, Outcome, RunResult, Source, StageResult, Status};
use crate::stage::{sdk_pin, Ctx};
use crate::util;

/// The speed-facts sidecar beside an installed disc: `<Name>.speed-facts.json`.
pub const FACTS_SUFFIX: &str = ".speed-facts.json";

/// The speed facts of one judged result, as stored beside the installed disc. JSON:
/// `{"schema": 1, "result_sha256": "<hex>", "windows": {"<row>": {"offsets": [0, 12],
/// "flips": [786, 778], "work_per_frame": [91234, 91301]}}}`, one entry per speed row (the
/// window name, or `<name>@<model>`), the three arrays parallel, one value per measured offset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedFacts {
    pub schema: u32,
    /// The result these facts were read from.
    pub result_sha256: String,
    pub windows: BTreeMap<String, FactsWindow>,
}

/// One speed row's per-offset numbers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactsWindow {
    pub offsets: Vec<i32>,
    pub flips: Vec<u64>,
    pub work_per_frame: Vec<u64>,
}

/// One offset's frames presented and work per frame, as read so far.
type FlipsAndWork = (Option<u64>, Option<u64>);

impl SpeedFacts {
    /// Read the speed stage's `<row>/<offset>/<metric>` facts out of a result.
    pub fn of(hash: &str, r: &RunResult) -> SpeedFacts {
        let mut by_row: BTreeMap<String, BTreeMap<i32, FlipsAndWork>> = BTreeMap::new();
        let facts = r
            .stages
            .iter()
            .filter(|s| s.name == "speed")
            .flat_map(|s| s.outcome.facts.iter());
        for (key, value) in facts {
            let mut parts = key.rsplitn(3, '/');
            let (Some(metric), Some(offset), Some(row)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(offset), Ok(value)) = (offset.parse::<i32>(), value.parse::<u64>()) else {
                continue;
            };
            let slot = by_row
                .entry(row.to_string())
                .or_default()
                .entry(offset)
                .or_default();
            match metric {
                "flips" => slot.0 = Some(value),
                "work_per_frame" => slot.1 = Some(value),
                _ => {}
            }
        }
        let mut windows = BTreeMap::new();
        for (row, offsets) in by_row {
            let mut w = FactsWindow::default();
            for (offset, (flips, work)) in offsets {
                if let (Some(flips), Some(work)) = (flips, work) {
                    w.offsets.push(offset);
                    w.flips.push(flips);
                    w.work_per_frame.push(work);
                }
            }
            if !w.offsets.is_empty() {
                windows.insert(row, w);
            }
        }
        SpeedFacts {
            schema: 1,
            result_sha256: hash.to_string(),
            windows,
        }
    }

    /// Read a facts file; `None` when there is none (the first install under this rule).
    pub fn load(path: &Path) -> Result<Option<SpeedFacts>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let facts: SpeedFacts =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if facts.schema != 1 {
            return Err(format!(
                "{}: schema {} is not 1",
                path.display(),
                facts.schema
            ));
        }
        Ok(Some(facts))
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

/// The install guarantee. `Ok` with the result's own speed facts (to store beside the installed
/// disc) when `r` (whose content hash is `hash`) may be installed over a build whose facts are
/// `installed` (`None`: no facts recorded yet, the first install under this rule, accepted):
///
/// - `r` is a complete full PASS (tier full, verdict PASS, nothing missing), of a clean tree,
///   on the install profile (a landing-profile result is refused);
/// - never slower than the last installed build: for every speed row the installed build was
///   judged on, over the offsets both measured, the mean work per frame is not above and the
///   mean frames presented not below the installed build's, within the strict bar's tolerance
///   (the installed build's own spread over its offsets, `WindowBase::tolerance`, no slack).
///   A row the installed build had and this result lacks is refused, not skipped.
//
// TODO(searched layout): the owner's install bar includes "searched layout". There is no cheap
// honest signal for it today: no game file declares its placement layout (no build sets
// `layout`), and the build stage only sees psoxide-pgo's bind line (a layout bound to this build),
// not whether that layout came from the placement search. Require it here once a build declares
// its layout file and psoxide-pgo reports the layout's search provenance.
pub fn check(
    hash: &str,
    r: &RunResult,
    installed: Option<&SpeedFacts>,
) -> Result<SpeedFacts, String> {
    let at = util::short(hash);
    if r.tier != "full" || r.verdict != Status::Pass || !r.landing_missing.is_empty() {
        return Err(format!(
            "result {at} is not a complete full PASS (tier {}, verdict {:?}{}): run \
             `psoxide-test run --tier full` first",
            r.tier,
            r.verdict,
            if r.landing_missing.is_empty() {
                String::new()
            } else {
                format!(", missing {}", r.landing_missing.join(", "))
            }
        ));
    }
    if r.dirty {
        return Err(format!(
            "result {at} judged a dirty tree: commit, then run the full tier again"
        ));
    }
    if !r.profile.is_install() {
        return Err(format!(
            "result {at} is a {}-profile PASS, not an install pass: run `psoxide-test run --tier \
             full` (the install profile) on these bytes",
            r.profile.name()
        ));
    }
    let facts = SpeedFacts::of(hash, r);
    let Some(installed) = installed else {
        return Ok(facts);
    };
    let from = util::short(&installed.result_sha256);
    for (row, old) in &installed.windows {
        let Some(new) = facts.windows.get(row) else {
            return Err(format!(
                "never slower than the installed build: window `{row}` was measured for the \
                 installed build (result {from}) but result {at} has no numbers for it"
            ));
        };
        let base = WindowBase {
            offsets: old.offsets.clone(),
            flips: old.flips.clone(),
            work_per_frame: old.work_per_frame.clone(),
            ..WindowBase::default()
        };
        let series = |values: &[u64]| -> Vec<(i32, u64)> {
            new.offsets
                .iter()
                .zip(values)
                .filter(|(o, _)| old.offsets.contains(o))
                .map(|(o, v)| (*o, *v))
                .collect()
        };
        let work = series(&new.work_per_frame);
        if work.is_empty() {
            return Err(format!(
                "never slower than the installed build: window `{row}` shares no offset with the \
                 installed build (installed {:?}, now {:?})",
                old.offsets, new.offsets
            ));
        }
        for (metric, now, values, higher) in [
            ("work per frame", work, &old.work_per_frame, false),
            ("frames presented", series(&new.flips), &old.flips, true),
        ] {
            let c = crate::stage::speed::judge_metric(metric, &now, &base, values, higher, 0);
            if c.status != Status::Pass {
                return Err(format!(
                    "never slower than the installed build: window `{row}` {metric} is {} for \
                     result {at}, the installed build (result {from}) had {}",
                    c.value, c.baseline
                ));
            }
        }
    }
    Ok(facts)
}

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
    let name = install
        .name
        .clone()
        .unwrap_or_else(|| ctx.game.name.clone());
    let installed = match SpeedFacts::load(&library.join(format!("{name}{FACTS_SUFFIX}"))) {
        Ok(f) => f,
        Err(e) => return result(Outcome::error("install", e)),
    };
    let facts = match check(&hash, &judged, installed.as_ref()).and_then(|f| f.to_json()) {
        Ok(f) => f,
        Err(e) => return result(Outcome::error("install", e)),
    };
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
    // The bytes that go into the library are the disc (bin and cue): those must be the judged
    // ones. Other files of `out` (a link map that names a temporary build directory) may differ.
    let installed = [b.disc.clone(), b.disc.replace(".cue", ".bin")];
    let differs: Vec<&String> = installed
        .iter()
        .filter(|f| !now.contains_key(*f) || now.get(*f) != judged_files.get(*f))
        .collect();
    if !differs.is_empty() {
        return result(Outcome::error(
            "install",
            format!(
                "{} in {} are not the bytes result {} judged: build and judge them again",
                differs
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                b.out,
                util::short(&hash)
            ),
        ));
    }
    // The one library layout (DESIGN.md, "Library layout"): flat in the library directory, as
    // psx-library documents it: `<Name>.bin`, `<Name>.cue`, `<Name>.provenance.json` and the map
    // as `<Name>.<pin12>.map`, listed under the provenance's `artifacts.map`.
    let games_dir = library.clone();
    let pin = sdk_pin(&ctx.repo, ctx.game);
    let source = match psx_library::Source::from_git(&ctx.repo, &ctx.game.repository) {
        Ok(s) => s,
        Err(e) => return result(Outcome::error("install", e.to_string())),
    };
    let map_bytes = b
        .map_path(&ctx.repo)
        .and_then(|m| std::fs::read(m).ok())
        .unwrap_or_default();
    let suffix = format!(".{}.map", util::short(&pin));
    let mut base = serde_json::json!({
        "harness": { "tool": "psoxide-test", "version": env!("CARGO_PKG_VERSION"),
                     "result_sha256": hash },
        "sdk_pin": pin,
        "emulator_gate_revision": GATE_REVISION,
        "files": now,
    });
    if !ctx.game.requires.is_empty() {
        // An image names the verdict each member stands on.
        base["requires"] = ctx
            .game
            .requires
            .iter()
            .map(|r| serde_json::json!({ "id": r.id, "result_sha256": r.result, "file": r.file }))
            .collect();
    }
    if !map_bytes.is_empty() {
        base["artifacts"] = serde_json::json!({
            "map": { "file": format!("{name}{suffix}"), "sha256": util::sha256_bytes(&map_bytes) }
        });
    }
    // The speed facts go in with the disc (written before the cue), so the next install is
    // judged against exactly this build.
    let mut sidecars = vec![psx_library::Sidecar::new(FACTS_SUFFIX, facts.as_bytes())];
    if !map_bytes.is_empty() {
        sidecars.push(psx_library::Sidecar::new(&suffix, &map_bytes));
    }
    let cue = b.cue(&ctx.repo);
    let request = psx_library::Request::new(&cue, &games_dir, &name, source)
        .with_provenance_base(&base)
        .with_sidecars(&sidecars);
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

/// `psoxide-test install-check --result RESULT.json --facts INSTALLED_FACTS.json
/// [--write-facts OUT.json]`: [`check`] for an install made outside this stage (the campaign's
/// install script). Exit 0 only when RESULT may be installed; a missing facts file is the first
/// install. `--write-facts` writes RESULT's speed facts for the caller to keep beside the disc.
pub fn check_command(args: &[String]) -> Result<std::process::ExitCode, String> {
    let (mut result, mut facts, mut write) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--result" => result = Some(value()?),
            "--facts" => facts = Some(value()?),
            "--write-facts" => write = Some(value()?),
            other => return Err(format!("install-check: unknown argument `{other}`")),
        }
    }
    let (Some(result), Some(facts)) = (result, facts) else {
        return Err(
            "usage: psoxide-test install-check --result RESULT.json --facts \
                    INSTALLED_FACTS.json [--write-facts OUT.json]"
                .into(),
        );
    };
    let (hash, r) = read_result(Path::new(&result))?;
    let installed = SpeedFacts::load(Path::new(&facts))?;
    match check(&hash, &r, installed.as_ref()) {
        Ok(new) => {
            if let Some(out) = write {
                std::fs::write(&out, new.to_json()?).map_err(|e| format!("write {out}: {e}"))?;
            }
            println!(
                "install-check PASS: result {} {}",
                util::short(&hash),
                if installed.is_some() {
                    "is not slower than the installed build"
                } else {
                    "(no installed speed facts: first install under this rule)"
                }
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("install-check REFUSED: {e}");
            Ok(std::process::ExitCode::FAILURE)
        }
    }
}

/// A result file by path: its content hash and contents. A file named `<sha256>.json` must hash
/// to its own name.
fn read_result(path: &Path) -> Result<(String, RunResult), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let hash = util::sha256_bytes(text.as_bytes());
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit()) && stem != hash {
        return Err(format!(
            "{} hashes to {}, not its name: it was edited",
            path.display(),
            util::short(&hash)
        ));
    }
    let r: RunResult =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((hash, r))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::Profile;

    /// A complete clean full install-profile PASS whose one window `w` measured `work` cycles
    /// and `flips` frames at offsets -12, 0 and 12.
    fn passing(work: [u64; 3], flips: [u64; 3]) -> RunResult {
        let mut facts = BTreeMap::new();
        for (i, offset) in [-12, 0, 12].into_iter().enumerate() {
            facts.insert(format!("w/{offset}/work_per_frame"), work[i].to_string());
            facts.insert(format!("w/{offset}/flips"), flips[i].to_string());
            facts.insert(format!("w/{offset}/vblanks"), "1:700".to_string());
        }
        facts.insert("w/polls".to_string(), "10..20".to_string());
        let mut r = RunResult {
            schema: 1,
            game: "g".into(),
            commit: "c".repeat(40),
            dirty: false,
            sdk_pin: "s".repeat(40),
            sdk_guest: String::new(),
            harness: "0.1.0".into(),
            gate_revision: "r".repeat(40),
            tier: "full".into(),
            profile: Profile::Install,
            stages: vec![StageResult {
                name: "speed".into(),
                source: Source::Run,
                key: String::new(),
                outcome: Outcome {
                    checks: vec![Check::pass("w cycles per frame", "")],
                    facts,
                },
            }],
            skips: Vec::new(),
            verdict: Status::Pass,
            landing_missing: Vec::new(),
        };
        r.finish();
        r
    }

    #[test]
    fn the_first_install_is_accepted_and_its_facts_recorded() {
        let r = passing([100, 102, 104], [790, 786, 782]);
        let facts = check("ab", &r, None).unwrap();
        assert_eq!(facts.result_sha256, "ab");
        assert_eq!(
            facts.windows["w"],
            FactsWindow {
                offsets: vec![-12, 0, 12],
                flips: vec![790, 786, 782],
                work_per_frame: vec![100, 102, 104],
            }
        );
        // Written and read back through the sidecar format; a missing file is the first install.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("G{FACTS_SUFFIX}"));
        assert_eq!(SpeedFacts::load(&path).unwrap(), None);
        std::fs::write(&path, facts.to_json().unwrap()).unwrap();
        assert_eq!(SpeedFacts::load(&path).unwrap(), Some(facts));
    }

    #[test]
    fn a_landing_profile_result_is_refused() {
        let mut r = passing([100, 102, 104], [790, 786, 782]);
        r.profile = Profile::Landing;
        let e = check("ab", &r, None).unwrap_err();
        assert!(
            e.contains("landing-profile PASS, not an install pass"),
            "{e}"
        );
    }

    #[test]
    fn an_incomplete_or_dirty_or_failed_result_is_refused() {
        let base = passing([100, 102, 104], [790, 786, 782]);
        let mut r = base.clone();
        r.landing_missing = vec!["tier full".into()];
        assert!(check("ab", &r, None)
            .unwrap_err()
            .contains("complete full PASS"));
        let mut r = base.clone();
        r.verdict = Status::Fail;
        assert!(check("ab", &r, None)
            .unwrap_err()
            .contains("complete full PASS"));
        let mut r = base;
        r.dirty = true;
        assert!(check("ab", &r, None).unwrap_err().contains("dirty"));
    }

    #[test]
    fn slower_than_the_installed_build_is_refused() {
        let installed = check("old", &passing([100, 102, 104], [790, 786, 782]), None).unwrap();
        // Spread over three offsets: work (104-100)/3, flips (790-782)/3. Inside it passes.
        let same = passing([101, 103, 105], [789, 785, 781]);
        assert!(check("new", &same, Some(&installed)).is_ok());
        let faster = passing([90, 91, 92], [795, 790, 788]);
        assert!(check("new", &faster, Some(&installed)).is_ok());
        let more_work = passing([103, 105, 107], [790, 786, 782]);
        let e = check("new", &more_work, Some(&installed)).unwrap_err();
        assert!(e.contains("window `w` work per frame is 105.0"), "{e}");
        assert!(e.contains("had 102.0"), "{e}");
        let fewer_flips = passing([100, 102, 104], [786, 782, 778]);
        let e = check("new", &fewer_flips, Some(&installed)).unwrap_err();
        assert!(e.contains("window `w` frames presented is 782.0"), "{e}");
        assert!(e.contains("had 786.0"), "{e}");
        // A window the installed build was judged on must be measured again.
        let mut gone = passing([100, 102, 104], [790, 786, 782]);
        gone.stages.clear();
        assert!(check("new", &gone, Some(&installed))
            .unwrap_err()
            .contains("has no numbers for it"));
    }

    #[test]
    fn install_check_reads_the_result_and_writes_the_facts() {
        let dir = tempfile::tempdir().unwrap();
        let write_result = |r: &RunResult| {
            let json = serde_json::to_string_pretty(r).unwrap();
            let path = dir
                .path()
                .join(format!("{}.json", util::sha256_bytes(json.as_bytes())));
            std::fs::write(&path, json).unwrap();
            path.display().to_string()
        };
        let facts = dir.path().join("installed.json").display().to_string();
        let out = dir.path().join("out.json").display().to_string();
        let args = |result: &str| -> Vec<String> {
            ["--result", result, "--facts", &facts, "--write-facts", &out]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        let first = write_result(&passing([100, 102, 104], [790, 786, 782]));
        assert_eq!(
            check_command(&args(&first)).unwrap(),
            std::process::ExitCode::SUCCESS
        );
        std::fs::rename(&out, &facts).unwrap();
        let slower = write_result(&passing([110, 112, 114], [790, 786, 782]));
        assert_eq!(
            check_command(&args(&slower)).unwrap(),
            std::process::ExitCode::FAILURE
        );
        let mut landing = passing([100, 102, 104], [790, 786, 782]);
        landing.profile = Profile::Landing;
        let landing = write_result(&landing);
        assert_eq!(
            check_command(&args(&landing)).unwrap(),
            std::process::ExitCode::FAILURE
        );
    }
}
