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

/// The search stamp, as psoxide-pgo defines it (tools/psoxide-pgo: `layout::SEARCHED_TAG`, and
/// its README, "The search stamp"). `psoxide-pgo search` writes it as the second line of every
/// layout profile it places:
///
/// ```text
/// # psoxide-pgo layout 1
/// # psoxide-pgo searched: rounds <N> seed <N>
/// ```
///
/// `order` and `layout` write layouts without it, so a stamped layout's placement came from the
/// search. Read here by this exact prefix, then `rounds N seed N`, in the profile's leading
/// comment lines.
pub const SEARCHED_TAG: &str = "# psoxide-pgo searched: ";

/// The first line of every psoxide-pgo layout profile.
pub const LAYOUT_MAGIC: &str = "# psoxide-pgo layout 1";

/// One committed layout profile of a build being installed.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedLayout {
    /// Its path in the repository.
    pub path: String,
    /// Its text at the judged commit.
    pub text: String,
}

/// Whether a layout profile's text carries the search stamp ([`SEARCHED_TAG`]).
#[must_use]
pub fn search_stamped(text: &str) -> bool {
    text.lines()
        .take_while(|line| line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(SEARCHED_TAG))
        .any(|rest| {
            let fields: Vec<&str> = rest.split(' ').collect();
            matches!(fields.as_slice(), ["rounds", rounds, "seed", seed]
                if rounds.parse::<u64>().is_ok() && seed.parse::<u64>().is_ok())
        })
}

/// The layout profiles `build` links with, as committed at `commit`: its `layout_files`, or
/// when it names none every tracked `*.layout` file whose first line is [`LAYOUT_MAGIC`]. A
/// build that links without its layout on purpose (`layout = false`) has none. A named file
/// that the commit lacks, or that is not a layout profile, is an error.
pub fn committed_layouts(
    repo: &Path,
    commit: &str,
    build: &crate::game::Build,
) -> Result<Vec<CommittedLayout>, String> {
    if build.layout == Some(false) {
        return Ok(Vec::new());
    }
    let declared = !build.layout_files.is_empty();
    let paths: Vec<String> = if declared {
        build.layout_files.clone()
    } else {
        util::git(repo, &["ls-tree", "-r", "--name-only", commit])?
            .lines()
            .filter(|path| path.ends_with(".layout"))
            .map(str::to_string)
            .collect()
    };
    let mut layouts = Vec::new();
    for path in paths {
        let text = util::git(repo, &["show", &format!("{commit}:{path}")]).map_err(|e| {
            format!(
                "build `{}` names layout file `{path}`, which commit {} does not hold: {e}",
                build.name,
                util::short(commit)
            )
        })?;
        if text.lines().next() == Some(LAYOUT_MAGIC) {
            layouts.push(CommittedLayout { path, text });
        } else if declared {
            return Err(format!(
                "build `{}` names layout file `{path}`, which is not a psoxide-pgo layout \
                 profile (no `{LAYOUT_MAGIC}` first line)",
                build.name
            ));
        }
    }
    Ok(layouts)
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
/// - a searched layout: every committed layout profile of the installed builds (`layouts`, from
///   [`committed_layouts`] at the result's commit) carries psoxide-pgo's search stamp
///   ([`SEARCHED_TAG`]); a game with no layout profile passes.
pub fn check(
    hash: &str,
    r: &RunResult,
    installed: Option<&SpeedFacts>,
    layouts: &[CommittedLayout],
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
    let greedy: Vec<&str> = layouts
        .iter()
        .filter(|l| !search_stamped(&l.text))
        .map(|l| l.path.as_str())
        .collect();
    if !greedy.is_empty() {
        return Err(format!(
            "result {at} links with a layout that `psoxide-pgo search` did not write (no \
             `{SEARCHED_TAG}rounds N seed N` line at commit {}): {}. Run the layout search on \
             this code (`psoxide-pgo search ... --start recorded` from the committed layout, or \
             the game's search step), commit the layout it writes, then bless and run the full \
             tier on that commit",
            util::short(&r.commit),
            greedy.join(", ")
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
    let layouts = match committed_layouts(&ctx.repo, &judged.commit, b) {
        Ok(l) => l,
        Err(e) => return result(Outcome::error("install", e)),
    };
    let facts = match check(&hash, &judged, installed.as_ref(), &layouts).and_then(|f| f.to_json())
    {
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

/// `psoxide-test install-check --result RESULT.json --facts INSTALLED_FACTS.json --repo DIR
/// [--write-facts OUT.json]`: [`check`] for an install made outside this stage (the campaign's
/// install script). Exit 0 only when RESULT may be installed; a missing facts file is the first
/// install. `--repo` is a clone of the game holding RESULT's commit: the layouts of the game
/// file's `[[install]]` builds are read there at that commit. `--write-facts` writes RESULT's
/// speed facts for the caller to keep beside the disc.
pub fn check_command(args: &[String]) -> Result<std::process::ExitCode, String> {
    let (mut result, mut facts, mut repo, mut write) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--result" => result = Some(value()?),
            "--facts" => facts = Some(value()?),
            "--repo" => repo = Some(value()?),
            "--write-facts" => write = Some(value()?),
            other => return Err(format!("install-check: unknown argument `{other}`")),
        }
    }
    let (Some(result), Some(facts), Some(repo)) = (result, facts, repo) else {
        return Err(
            "usage: psoxide-test install-check --result RESULT.json --facts \
                    INSTALLED_FACTS.json --repo GAME_CLONE [--write-facts OUT.json]"
                .into(),
        );
    };
    let (hash, r) = read_result(Path::new(&result))?;
    let installed = SpeedFacts::load(Path::new(&facts))?;
    let layouts = install_layouts(Path::new(&repo), &r.commit)?;
    match check(&hash, &r, installed.as_ref(), &layouts) {
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

/// The committed layouts of every `[[install]]` build in the game file at `commit` in `repo`.
fn install_layouts(repo: &Path, commit: &str) -> Result<Vec<CommittedLayout>, String> {
    let file = format!("{commit}:{}", crate::game::GAME_FILE);
    let text = util::git(repo, &["show", &file]).map_err(|e| {
        format!(
            "--repo {}: no game file at {}: {e}",
            repo.display(),
            util::short(commit)
        )
    })?;
    let game = crate::game::Game::parse(&text).map_err(|e| format!("{file}: {e}"))?;
    if game.installs.is_empty() {
        return Err(format!("{file} declares no [[install]]"));
    }
    let mut layouts = Vec::new();
    for install in &game.installs {
        let build = game.build(&install.build).ok_or_else(|| {
            format!(
                "{file}: [[install]] names unknown build `{}`",
                install.build
            )
        })?;
        for layout in committed_layouts(repo, commit, build)? {
            if !layouts.contains(&layout) {
                layouts.push(layout);
            }
        }
    }
    Ok(layouts)
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
        let facts = check("ab", &r, None, &[]).unwrap();
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
        let e = check("ab", &r, None, &[]).unwrap_err();
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
        assert!(check("ab", &r, None, &[])
            .unwrap_err()
            .contains("complete full PASS"));
        let mut r = base.clone();
        r.verdict = Status::Fail;
        assert!(check("ab", &r, None, &[])
            .unwrap_err()
            .contains("complete full PASS"));
        let mut r = base;
        r.dirty = true;
        assert!(check("ab", &r, None, &[]).unwrap_err().contains("dirty"));
    }

    #[test]
    fn slower_than_the_installed_build_is_refused() {
        let installed =
            check("old", &passing([100, 102, 104], [790, 786, 782]), None, &[]).unwrap();
        // Spread over three offsets: work (104-100)/3, flips (790-782)/3. Inside it passes.
        let same = passing([101, 103, 105], [789, 785, 781]);
        assert!(check("new", &same, Some(&installed), &[]).is_ok());
        let faster = passing([90, 91, 92], [795, 790, 788]);
        assert!(check("new", &faster, Some(&installed), &[]).is_ok());
        let more_work = passing([103, 105, 107], [790, 786, 782]);
        let e = check("new", &more_work, Some(&installed), &[]).unwrap_err();
        assert!(e.contains("window `w` work per frame is 105.0"), "{e}");
        assert!(e.contains("had 102.0"), "{e}");
        let fewer_flips = passing([100, 102, 104], [786, 782, 778]);
        let e = check("new", &fewer_flips, Some(&installed), &[]).unwrap_err();
        assert!(e.contains("window `w` frames presented is 782.0"), "{e}");
        assert!(e.contains("had 786.0"), "{e}");
        // A window the installed build was judged on must be measured again.
        let mut gone = passing([100, 102, 104], [790, 786, 782]);
        gone.stages.clear();
        assert!(check("new", &gone, Some(&installed), &[])
            .unwrap_err()
            .contains("has no numbers for it"));
    }

    /// A git repository holding a game file (one build `b`, installed; `extra` is appended to
    /// its `[[build]]`) and `files`, committed. Returns its path and the commit.
    fn game_repo(dir: &Path, extra: &str, files: &[(&str, &str)]) -> (std::path::PathBuf, String) {
        let repo = dir.join("game");
        std::fs::create_dir_all(&repo).unwrap();
        let game = format!(
            "schema = 1\nname = \"g\"\nrepository = \"o/g\"\n[[build]]\nname = \"b\"\n\
             out = \"o\"\ncommands = [[\"true\"]]\ndisc = \"g.cue\"\n{extra}\n\
             [[install]]\nbuild = \"b\"\n"
        );
        std::fs::write(repo.join(crate::game::GAME_FILE), game).unwrap();
        for (path, text) in files {
            let path = repo.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(status.status.success(), "{status:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c"]);
        let commit = util::git(&repo, &["rev-parse", "HEAD"]).unwrap();
        (repo, commit)
    }

    const GREEDY: &str = "# psoxide-pgo layout 1\n# features: (default)\n# variant: profi\n\
                          f 0000000000000001 1 a\nw 0:5\n";
    const SEARCHED: &str = "# psoxide-pgo layout 1\n# psoxide-pgo searched: rounds 600 seed 1\n\
                            # features: (default)\n# variant: profi\nf 0000000000000001 1 a\n\
                            w 0:5\n# placement: psoxide-pgo search, seed 1 rounds 600\n\
                            s 80010000\no 0000000000000001 1 80010000 a\n";

    #[test]
    fn the_search_stamp_is_read_from_the_header_only() {
        assert!(search_stamped(SEARCHED));
        assert!(!search_stamped(GREEDY));
        // The pre-stamp record of a search is not the stamp.
        assert!(!search_stamped(
            &SEARCHED.replace("# psoxide-pgo searched: rounds 600 seed 1\n", "")
        ));
        // Below the header, or malformed, it does not count.
        assert!(!search_stamped(&format!(
            "{GREEDY}# psoxide-pgo searched: rounds 600 seed 1\n"
        )));
        for bad in [
            "rounds 600",
            "rounds x seed 1",
            "seed 1 rounds 600",
            "rounds 6 seed 1 x",
        ] {
            assert!(
                !search_stamped(&format!("{LAYOUT_MAGIC}\n{SEARCHED_TAG}{bad}\n")),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_install_whose_layout_was_not_searched_is_refused() {
        let r = passing([100, 102, 104], [790, 786, 782]);
        let layout = |path: &str, text: &str| CommittedLayout {
            path: path.into(),
            text: text.into(),
        };
        // No layout at all: nothing to search.
        assert!(check("ab", &r, None, &[]).is_ok());
        assert!(check("ab", &r, None, &[layout("pgo/g.layout", SEARCHED)]).is_ok());
        let e = check(
            "ab",
            &r,
            None,
            &[
                layout("pgo/g.layout", SEARCHED),
                layout("pgo/t.layout", GREEDY),
            ],
        )
        .unwrap_err();
        assert!(
            e.contains("did not write") && e.contains("pgo/t.layout"),
            "{e}"
        );
        assert!(!e.contains("pgo/g.layout"), "{e}");
        assert!(e.contains("psoxide-pgo search"), "{e}");
    }

    #[test]
    fn the_committed_layouts_are_the_declared_ones_or_every_tracked_layout_profile() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, commit) = game_repo(
            dir.path(),
            "",
            &[
                ("pgo/g.layout", SEARCHED),
                ("pgo/t.layout", GREEDY),
                ("docs/not-a-profile.layout", "some other format\n"),
            ],
        );
        let game = crate::game::Game::load(&repo).unwrap();
        let mut b = game.builds.into_iter().next().unwrap();
        let paths = |b: &crate::game::Build| -> Vec<String> {
            committed_layouts(&repo, &commit, b)
                .unwrap()
                .into_iter()
                .map(|l| l.path)
                .collect()
        };
        assert_eq!(paths(&b), ["pgo/g.layout", "pgo/t.layout"]);
        // A working-tree edit is not the commit's layout.
        std::fs::write(repo.join("pgo/t.layout"), SEARCHED).unwrap();
        let t = committed_layouts(&repo, &commit, &b).unwrap();
        assert_eq!(t[1].text.trim_end(), GREEDY.trim_end());
        b.layout_files = vec!["pgo/g.layout".into()];
        assert_eq!(paths(&b), ["pgo/g.layout"]);
        b.layout_files = vec!["pgo/gone.layout".into()];
        assert!(committed_layouts(&repo, &commit, &b)
            .unwrap_err()
            .contains("does not hold"));
        b.layout_files = vec!["docs/not-a-profile.layout".into()];
        assert!(committed_layouts(&repo, &commit, &b)
            .unwrap_err()
            .contains("not a psoxide-pgo layout profile"));
        b.layout_files.clear();
        b.layout = Some(false);
        assert!(paths(&b).is_empty());
    }

    #[test]
    fn install_check_reads_the_result_and_writes_the_facts() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, commit) = game_repo(dir.path(), "", &[("pgo/g.layout", SEARCHED)]);
        let repo = repo.display().to_string();
        let write_result = |r: &RunResult| {
            let mut r = r.clone();
            r.commit = commit.clone();
            let json = serde_json::to_string_pretty(&r).unwrap();
            let path = dir
                .path()
                .join(format!("{}.json", util::sha256_bytes(json.as_bytes())));
            std::fs::write(&path, json).unwrap();
            path.display().to_string()
        };
        let facts = dir.path().join("installed.json").display().to_string();
        let out = dir.path().join("out.json").display().to_string();
        let args = |result: &str, repo: &str| -> Vec<String> {
            [
                "--result",
                result,
                "--facts",
                &facts,
                "--repo",
                repo,
                "--write-facts",
                &out,
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        };
        let first = write_result(&passing([100, 102, 104], [790, 786, 782]));
        assert_eq!(
            check_command(&args(&first, &repo)).unwrap(),
            std::process::ExitCode::SUCCESS
        );
        std::fs::rename(&out, &facts).unwrap();
        let slower = write_result(&passing([110, 112, 114], [790, 786, 782]));
        assert_eq!(
            check_command(&args(&slower, &repo)).unwrap(),
            std::process::ExitCode::FAILURE
        );
        let mut landing = passing([100, 102, 104], [790, 786, 782]);
        landing.profile = Profile::Landing;
        let landing = write_result(&landing);
        assert_eq!(
            check_command(&args(&landing, &repo)).unwrap(),
            std::process::ExitCode::FAILURE
        );
        // Without --repo the layout cannot be checked: refused as a usage error.
        let mut no_repo = args(&first, &repo);
        no_repo.drain(4..6);
        assert!(check_command(&no_repo).unwrap_err().contains("--repo"));
        // The same result against a clone whose layout at that commit is greedy is refused.
        let greedy_dir = tempfile::tempdir().unwrap();
        let (greedy_repo, greedy_commit) =
            game_repo(greedy_dir.path(), "", &[("pgo/g.layout", GREEDY)]);
        let mut r = passing([100, 102, 104], [790, 786, 782]);
        r.commit = greedy_commit;
        let json = serde_json::to_string_pretty(&r).unwrap();
        let path = greedy_dir.path().join("r.json");
        std::fs::write(&path, json).unwrap();
        assert_eq!(
            check_command(&args(
                &path.display().to_string(),
                &greedy_repo.display().to_string()
            ))
            .unwrap(),
            std::process::ExitCode::FAILURE
        );
        // A repo without the result's commit is an error, never a pass.
        assert!(check_command(&args(&first, &greedy_repo.display().to_string())).is_err());
    }
}
