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
//! Those facts name the disc they describe and each window's identity, so they are only compared
//! with a result that measured the same windows over the disc the library holds.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::baseline::WindowBase;
use crate::config::GATE_REVISION;
use crate::game::Game;
use crate::report::{Check, Outcome, RunResult, Source, StageResult, Status, RESULT_SCHEMA_MAX};
use crate::stage::{sdk_pin, Ctx};
use crate::util;

/// The speed-facts sidecar beside an installed disc: `<Name>.speed-facts.json`.
pub const FACTS_SUFFIX: &str = ".speed-facts.json";

/// The speed-facts schema this harness writes. 1 (read, never written): no disc binding, no
/// window identity, no p99; such facts can only be replaced with `--rebase-facts`.
pub const FACTS_SCHEMA: u32 = 2;

/// The `install-check --stamp` format.
pub const STAMP_SCHEMA: u32 = 1;

/// The speed facts of one judged result, as stored beside the installed disc. JSON:
///
/// ```text
/// {"schema": 2, "result_sha256": "<hex>", "bin_sha256": "<hex>", "map_sha256": "<hex>"|null,
///  "windows": {"<row>": {"offsets": [-12, 0, 12], "flips": [...], "work_per_frame": [...],
///                        "frame_work_p99": [...], "identity": {...}}}}
/// ```
///
/// one entry per speed row (the window name, or `<name>@<model>`), the value arrays parallel to
/// `offsets`. `bin_sha256` and `map_sha256` are the installed disc's; `identity` is
/// [`WindowIdentity`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedFacts {
    pub schema: u32,
    /// The result these facts were read from.
    pub result_sha256: String,
    /// The installed bin these facts describe (empty only in schema 1).
    #[serde(default)]
    pub bin_sha256: String,
    /// The installed link map (`None`: the build declares none, or schema 1).
    #[serde(default)]
    pub map_sha256: Option<String>,
    pub windows: BTreeMap<String, FactsWindow>,
}

/// One speed row's per-offset numbers and what was measured.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactsWindow {
    pub offsets: Vec<i32>,
    pub flips: Vec<u64>,
    pub work_per_frame: Vec<u64>,
    /// Empty when the result had no p99 at some offset (and in schema 1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_work_p99: Vec<u64>,
    /// `None` only in schema 1 (or for a row the game file no longer declares).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<WindowIdentity>,
}

/// What a speed row measured. Two rows are compared only when their identities are equal.
/// The frontend and psoxide-pgo binaries are not in a result (only in the speed receipts' keys),
/// so the emulator is named by the result's gate revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowIdentity {
    /// The tape, relative to the game repo, and the sha256 of its bytes at the judged commit.
    pub tape: Option<String>,
    pub tape_sha256: Option<String>,
    /// `polls A..B` for a declared window, or `located from <cond> until <cond> max_polls N` for
    /// one found by conditions (the located polls move with the code; the locator does not).
    pub window: String,
    /// The declared phase offsets.
    pub offsets: Vec<i32>,
    /// The frontend arguments of the replay (hold-last included).
    pub launch_args: Vec<String>,
    /// The emulator gate revision the result was judged with.
    pub gate_revision: String,
}

impl WindowIdentity {
    /// The fields that differ from `other`, described.
    pub(crate) fn differences(&self, other: &WindowIdentity) -> Vec<String> {
        let mut out = Vec::new();
        let mut field = |name: &str, a: String, b: String| {
            if a != b {
                out.push(format!("{name} {a} -> {b}"));
            }
        };
        field(
            "tape",
            format!("{:?}", self.tape),
            format!("{:?}", other.tape),
        );
        field(
            "tape sha256",
            self.tape_sha256
                .as_deref()
                .map_or("none", util::short)
                .into(),
            other
                .tape_sha256
                .as_deref()
                .map_or("none", util::short)
                .into(),
        );
        field("window", self.window.clone(), other.window.clone());
        field(
            "offsets",
            format!("{:?}", self.offsets),
            format!("{:?}", other.offsets),
        );
        field(
            "launch args",
            format!("{:?}", self.launch_args),
            format!("{:?}", other.launch_args),
        );
        field(
            "gate revision",
            util::short(&self.gate_revision).into(),
            util::short(&other.gate_revision).into(),
        );
        out
    }
}

/// The identity of every speed row `game` declares, as committed at `commit` in `repo` (the tape
/// bytes are read there), judged with `gate_revision`.
pub fn window_identities(
    repo: &Path,
    commit: &str,
    game: &Game,
    gate_revision: &str,
) -> Result<BTreeMap<String, WindowIdentity>, String> {
    let mut out = BTreeMap::new();
    for w in &game.windows {
        let tape_sha256 = match &w.tape {
            Some(t) => Some(util::git_blob_sha256(repo, commit, t).map_err(|e| {
                format!(
                    "[[speed]] `{}`: tape `{t}` at commit {}: {e}",
                    w.name,
                    util::short(commit)
                )
            })?),
            None => None,
        };
        let condition = |c: &Option<toml::Table>| {
            c.as_ref().map_or("none".into(), |t| {
                serde_json::to_string(t).unwrap_or_default()
            })
        };
        let window = match w.polls {
            Some([a, b]) => format!("polls {a}..{b}"),
            None => format!(
                "located from {} until {} max_polls {}",
                condition(&w.from),
                condition(&w.until),
                w.max_polls.unwrap_or(0)
            ),
        };
        let identity = WindowIdentity {
            tape: w.tape.clone(),
            tape_sha256,
            window,
            offsets: w.offsets.clone(),
            launch_args: w.frontend_args(),
            gate_revision: gate_revision.to_string(),
        };
        for (row, _) in w.rows() {
            out.insert(row, identity.clone());
        }
    }
    Ok(out)
}

/// One offset's frames presented, work per frame and p99, as read so far.
type OffsetNumbers = (Option<u64>, Option<u64>, Option<u64>);

impl SpeedFacts {
    /// Read the speed stage's `<row>/<offset>/<metric>` facts out of the judged result, bound to
    /// its disc and each row's identity.
    pub fn of(j: &Judged<'_>) -> SpeedFacts {
        let mut by_row: BTreeMap<String, BTreeMap<i32, OffsetNumbers>> = BTreeMap::new();
        let facts = j
            .result
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
                "frame_work_p99" => slot.2 = Some(value),
                _ => {}
            }
        }
        let mut windows = BTreeMap::new();
        for (row, offsets) in by_row {
            let mut w = FactsWindow::default();
            let mut p99 = Vec::new();
            for (offset, (flips, work, worst)) in offsets {
                if let (Some(flips), Some(work)) = (flips, work) {
                    w.offsets.push(offset);
                    w.flips.push(flips);
                    w.work_per_frame.push(work);
                    p99.push(worst);
                }
            }
            if w.offsets.is_empty() {
                continue;
            }
            w.frame_work_p99 = p99
                .into_iter()
                .collect::<Option<Vec<u64>>>()
                .unwrap_or_default();
            w.identity = j.identities.get(&row).cloned();
            windows.insert(row, w);
        }
        SpeedFacts {
            schema: FACTS_SCHEMA,
            result_sha256: j.hash.to_string(),
            bin_sha256: j.bin_sha256.to_string(),
            map_sha256: j.map_sha256.map(str::to_string),
            windows,
        }
    }

    /// Read a facts file; `None` when there is none.
    pub fn load(path: &Path) -> Result<Option<SpeedFacts>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let facts: SpeedFacts =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if facts.schema == 0 || facts.schema > FACTS_SCHEMA {
            return Err(format!(
                "{}: speed-facts schema {} is not one this harness reads (1..={FACTS_SCHEMA})",
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

/// Fewest search rounds an installed layout may carry in its stamp: the psoxide-pgo README's
/// recipe and the floor tools/pgo-search-flow refuses under. A search stopped after a round or
/// two measures like no search (one landed with `rounds 1`, 11-22) but carries the same stamp.
pub const MIN_SEARCH_ROUNDS: u64 = 600;

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
    search_stamp(text).is_some()
}

/// The search stamp's `(rounds, seed)`, when a layout profile's text carries it.
#[must_use]
pub fn search_stamp(text: &str) -> Option<(u64, u64)> {
    text.lines()
        .take_while(|line| line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(SEARCHED_TAG))
        .find_map(|rest| {
            let fields: Vec<&str> = rest.split(' ').collect();
            match fields.as_slice() {
                ["rounds", rounds, "seed", seed] => {
                    Some((rounds.parse().ok()?, seed.parse().ok()?))
                }
                _ => None,
            }
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

/// The judged side of [`check`]: the result, its content hash, and what is about to be installed.
pub struct Judged<'a> {
    pub hash: &'a str,
    pub result: &'a RunResult,
    /// The committed layout profiles of the installed builds ([`committed_layouts`]).
    pub layouts: &'a [CommittedLayout],
    /// Every declared speed row's identity ([`window_identities`]).
    pub identities: &'a BTreeMap<String, WindowIdentity>,
    /// The bin and link map being installed (already matched with the result's build stage).
    pub bin_sha256: &'a str,
    pub map_sha256: Option<&'a str>,
}

/// What the library holds under the name being installed.
#[derive(Debug, Clone, Copy)]
pub enum Installed<'a> {
    /// No disc and no speed facts: the first install.
    Nothing,
    /// A disc but no speed facts describing it.
    DiscWithoutFacts,
    /// Speed facts, and the sha256 of the bin the library holds now.
    Facts {
        facts: &'a SpeedFacts,
        library_bin_sha256: &'a str,
    },
}

/// An accepted install: the facts to store beside the disc, and what was judged (printed).
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub facts: SpeedFacts,
    pub note: String,
    /// `--rebase-facts` took this result as the new reference without comparing it.
    pub rebased: bool,
}

/// The install guarantee. `Ok` with the result's own speed facts (to store beside the installed
/// disc) when the judged result may replace what the library holds (`installed`):
///
/// - the result's schema is one this harness knows (at most [`RESULT_SCHEMA_MAX`]);
/// - it is a complete full PASS (tier full, verdict PASS, nothing missing), of a clean tree,
///   on the install profile (a landing-profile result is refused);
/// - a searched layout: every committed layout profile of the installed builds carries
///   psoxide-pgo's search stamp ([`SEARCHED_TAG`]) with at least [`MIN_SEARCH_ROUNDS`] rounds;
///   a game with no layout profile passes;
/// - never slower than the installed build: only [`Installed::Nothing`] is a first install.
///   The installed facts must describe the library's bin (`bin_sha256`). For every row they
///   hold whose [`WindowIdentity`] equals this result's, over the offsets both measured, the
///   mean work per frame and p99 are not above and the mean frames presented not below the
///   installed build's, within the installed build's own spread over its offsets (the strict
///   bar, `WindowBase::tolerance`, no slack); such a row this result lacks is refused.
///
/// Facts that cannot be compared (a disc without facts, schema-1 facts, facts of another bin,
/// a row whose identity changed) are refused, unless `rebase`: then the result, which passed
/// everything above, becomes the new reference and the note says so loudly. `rebase` never
/// waives a comparison that can be made.
pub fn check(j: &Judged<'_>, installed: Installed<'_>, rebase: bool) -> Result<Accepted, String> {
    let r = j.result;
    let at = util::short(j.hash);
    if r.schema > RESULT_SCHEMA_MAX {
        return Err(format!(
            "result {at} has schema {}, newer than this psoxide-test reads ({RESULT_SCHEMA_MAX}): \
             use the psoxide-test that wrote it",
            r.schema
        ));
    }
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
    let greedy: Vec<&str> = j
        .layouts
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
    let short: Vec<String> = j
        .layouts
        .iter()
        .filter_map(|l| {
            let (rounds, seed) = search_stamp(&l.text)?;
            (rounds < MIN_SEARCH_ROUNDS)
                .then(|| format!("{} (rounds {rounds} seed {seed})", l.path))
        })
        .collect();
    if !short.is_empty() {
        return Err(format!(
            "result {at} links with a layout searched for fewer than {MIN_SEARCH_ROUNDS} rounds \
             at commit {}: {}. A short search places like no search; run it for at least \
             {MIN_SEARCH_ROUNDS} rounds with no early stop (tools/pgo-search-flow), commit the \
             layout, then bless and run the full tier on that commit",
            util::short(&r.commit),
            short.join(", ")
        ));
    }
    let facts = SpeedFacts::of(j);
    let incomparable = |facts: SpeedFacts, why: String| -> Result<Accepted, String> {
        if rebase {
            Ok(Accepted {
                facts,
                note: format!(
                    "REBASED SPEED REFERENCE: {why}. --rebase-facts takes result {at}, a strict \
                     install-profile PASS, as the new reference: never-slower was NOT judged \
                     for this install"
                ),
                rebased: true,
            })
        } else {
            Err(format!(
                "never slower than the installed build cannot be judged: {why}. If that is \
                 intended (a moved window, a new tape or emulator, a disc installed without \
                 facts), pass --rebase-facts to take this strict PASS as the new reference"
            ))
        }
    };
    let (old, library_bin) = match installed {
        Installed::Nothing => {
            return Ok(Accepted {
                facts,
                note: "first install: the library holds neither a disc nor speed facts under \
                       this name"
                    .into(),
                rebased: false,
            })
        }
        Installed::DiscWithoutFacts => {
            return incomparable(
                facts,
                "the library holds a disc under this name but no speed facts describe it".into(),
            )
        }
        Installed::Facts {
            facts: old,
            library_bin_sha256,
        } => (old, library_bin_sha256),
    };
    let from = util::short(&old.result_sha256);
    if old.schema < 2 {
        return incomparable(
            facts,
            format!(
                "the installed speed facts (result {from}) are schema {}: they name neither the \
                 disc they describe nor what each window measured",
                old.schema
            ),
        );
    }
    if old.bin_sha256 != library_bin {
        return incomparable(
            facts,
            format!(
                "the installed speed facts (result {from}) describe bin {}, but the library's \
                 bin is {}: they do not describe what is installed",
                util::short(&old.bin_sha256),
                util::short(library_bin)
            ),
        );
    }
    let mut moved = Vec::new();
    for (row, old_w) in &old.windows {
        let now_id = j.identities.get(row);
        if old_w.identity.is_none() || old_w.identity.as_ref() != now_id {
            moved.push(match (&old_w.identity, now_id) {
                (Some(a), Some(b)) => format!("`{row}` ({})", a.differences(b).join(", ")),
                (_, None) => format!("`{row}` (no longer declared)"),
                (None, Some(_)) => format!("`{row}` (installed facts record no identity)"),
            });
            continue;
        }
        let Some(new) = facts.windows.get(row) else {
            return Err(format!(
                "never slower than the installed build: window `{row}` was measured for the \
                 installed build (result {from}) but result {at} has no numbers for it"
            ));
        };
        let base = WindowBase {
            offsets: old_w.offsets.clone(),
            flips: old_w.flips.clone(),
            work_per_frame: old_w.work_per_frame.clone(),
            frame_work_p99: old_w.frame_work_p99.clone(),
            ..WindowBase::default()
        };
        let series = |values: &[u64]| -> Vec<(i32, u64)> {
            new.offsets
                .iter()
                .zip(values)
                .filter(|(o, _)| old_w.offsets.contains(o))
                .map(|(o, v)| (*o, *v))
                .collect()
        };
        let work = series(&new.work_per_frame);
        if work.is_empty() {
            return Err(format!(
                "never slower than the installed build: window `{row}` shares no offset with the \
                 installed build (installed {:?}, now {:?})",
                old_w.offsets, new.offsets
            ));
        }
        let mut metrics = vec![
            ("work per frame", work, &old_w.work_per_frame, false),
            ("frames presented", series(&new.flips), &old_w.flips, true),
        ];
        if !old_w.frame_work_p99.is_empty() {
            if new.frame_work_p99.is_empty() {
                return Err(format!(
                    "never slower than the installed build: window `{row}` has a p99 for the \
                     installed build (result {from}) but none in result {at}"
                ));
            }
            metrics.push((
                "work per frame p99",
                series(&new.frame_work_p99),
                &old_w.frame_work_p99,
                false,
            ));
        }
        for (metric, now, values, higher) in metrics {
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
    if !moved.is_empty() {
        return incomparable(
            facts,
            format!(
                "these windows measure something else than when the installed build (result \
                 {from}) was judged: {}",
                moved.join("; ")
            ),
        );
    }
    Ok(Accepted {
        facts,
        note: format!(
            "not slower than the installed build (result {from}){}",
            if rebase {
                "; --rebase-facts was not needed and changed nothing"
            } else {
                ""
            }
        ),
        rebased: false,
    })
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

/// What the library holds under `name`: its speed facts, its bin's sha256, and whether any part
/// of a disc (bin or cue) is there.
fn library_state(
    library: &Path,
    name: &str,
) -> Result<(Option<SpeedFacts>, Option<String>, bool), String> {
    let facts = SpeedFacts::load(&library.join(format!("{name}{FACTS_SUFFIX}")))?;
    let bin = library.join(format!("{name}.bin"));
    let cue = library.join(format!("{name}.cue"));
    let bin_sha = if bin.is_file() {
        Some(util::sha256_file(&bin)?)
    } else {
        None
    };
    let disc = bin_sha.is_some() || cue.exists();
    Ok((facts, bin_sha, disc))
}

/// The install stage's reading of the library: no facts and no disc is the first install; a
/// disc (bin or cue) without facts is [`Installed::DiscWithoutFacts`]; facts without a bin are
/// an error (they describe nothing installed).
fn library_installed<'a>(
    facts: Option<&'a SpeedFacts>,
    bin_sha256: Option<&'a str>,
    disc: bool,
) -> Result<Installed<'a>, String> {
    match (facts, bin_sha256) {
        (None, _) if !disc => Ok(Installed::Nothing),
        (None, _) => Ok(Installed::DiscWithoutFacts),
        (Some(facts), Some(bin)) => Ok(Installed::Facts {
            facts,
            library_bin_sha256: bin,
        }),
        (Some(_), None) => Err(
            "speed facts but no bin: the facts describe nothing installed; restore the disc or \
             move the facts away"
                .into(),
        ),
    }
}

fn one(ctx: &Ctx<'_>, install: &crate::game::Install) -> StageResult {
    let result = |outcome| StageResult {
        name: "install".into(),
        source: Source::Run,
        key: String::new(),
        outcome,
    };
    let refuse = |e: String| result(Outcome::error("install", e));
    let Some(library) = &ctx.config.library else {
        return refuse("no library in the psoxide-test config: set `library`".into());
    };
    let Some(b) = ctx.game.build(&install.build) else {
        return refuse("unknown build".into());
    };
    let results = ctx.work.join("results");
    let (hash, judged) = match latest(&results) {
        Ok(v) => v,
        Err(e) => return refuse(e),
    };
    let name = install
        .name
        .clone()
        .unwrap_or_else(|| ctx.game.name.clone());
    let now = ctx.artifacts.get(&b.name).cloned().unwrap_or_default();
    let judged_files: BTreeMap<String, String> = judged
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
    let bin_name = b.disc.replace(".cue", ".bin");
    let disc_files = [b.disc.clone(), bin_name.clone()];
    let differs: Vec<&String> = disc_files
        .iter()
        .filter(|f| !now.contains_key(*f) || now.get(*f) != judged_files.get(*f))
        .collect();
    if !differs.is_empty() {
        return refuse(format!(
            "{} in {} are not the bytes result {} judged: build and judge them again",
            differs
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            b.out,
            util::short(&hash)
        ));
    }
    let bin_sha256 = now.get(&bin_name).cloned().unwrap_or_default();
    let map_bytes = b
        .map_path(&ctx.repo)
        .and_then(|m| std::fs::read(m).ok())
        .unwrap_or_default();
    let map_sha256 = (!map_bytes.is_empty()).then(|| util::sha256_bytes(&map_bytes));
    let (installed_facts, library_bin, library_disc) = match library_state(library, &name) {
        Ok(s) => s,
        Err(e) => return refuse(e),
    };
    let installed = match library_installed(
        installed_facts.as_ref(),
        library_bin.as_deref(),
        library_disc,
    ) {
        Ok(i) => i,
        Err(e) => return refuse(format!("library entry `{name}`: {e}")),
    };
    let layouts = match committed_layouts(&ctx.repo, &judged.commit, b) {
        Ok(l) => l,
        Err(e) => return refuse(e),
    };
    let identities =
        match window_identities(&ctx.repo, &judged.commit, ctx.game, &judged.gate_revision) {
            Ok(i) => i,
            Err(e) => return refuse(e),
        };
    let judged_side = Judged {
        hash: &hash,
        result: &judged,
        layouts: &layouts,
        identities: &identities,
        bin_sha256: &bin_sha256,
        map_sha256: map_sha256.as_deref(),
    };
    let accepted = match check(&judged_side, installed, ctx.rebase_facts) {
        Ok(a) => a,
        Err(e) => return refuse(e),
    };
    if accepted.rebased {
        eprintln!("\n*** {} ***\n", accepted.note);
    }
    let facts = match accepted.facts.to_json() {
        Ok(f) => f,
        Err(e) => return refuse(e),
    };
    // The one library layout (DESIGN.md, "Library layout"): flat in the library directory, as
    // psx-library documents it: `<Name>.bin`, `<Name>.cue`, `<Name>.provenance.json` and the map
    // as `<Name>.<pin12>.map`, listed under the provenance's `artifacts.map`.
    let games_dir = library.clone();
    let pin = sdk_pin(&ctx.repo, ctx.game);
    let source = match psx_library::Source::from_git(&ctx.repo, &ctx.game.repository) {
        Ok(s) => s,
        Err(e) => return refuse(e.to_string()),
    };
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
    if let Some(map_sha) = &map_sha256 {
        base["artifacts"] = serde_json::json!({
            "map": { "file": format!("{name}{suffix}"), "sha256": map_sha }
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
            .detail(format!("{}; {}", done.provenance.display(), accepted.note))],
            ..Outcome::default()
        }),
        Err(e) => refuse(e.to_string()),
    }
}

/// The marker `install-check --stamp` writes on success: a deploy script copies a bin only with
/// a stamp whose `bin_sha256` is that bin's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    /// [`STAMP_SCHEMA`].
    pub schema: u32,
    pub result_sha256: String,
    pub bin_sha256: String,
    pub map_sha256: Option<String>,
    /// Always `install`: only an install-profile result is ever stamped.
    pub profile: String,
    /// RFC 3339 UTC.
    pub checked_at: String,
    /// The new speed facts, to store beside the disc as `<Name>.speed-facts.json`.
    pub facts: SpeedFacts,
}

/// `psoxide-test install-check` (main.rs, usage): [`check`] for an install made outside this stage
/// (the campaign's install script).
///
/// - `--result` is a result file named by its own sha256; `--repo` a clone of the game holding
///   its commit (the game file, layouts and tapes are read there, at that commit).
/// - `--bin` and `--map` are the files about to be installed: the bin must be the one an
///   `[[install]]` build's bin the result's build stage recorded, the map that build's recorded
///   map (`--map` is required exactly when the build declares a map).
/// - `--facts` is the library's `<Name>.speed-facts.json` and `--library-bin` its `<Name>.bin`.
///   A missing facts file is refused (a typo must not disable never-slower) unless the library
///   bin exists, when it is a disc without facts (refused unless `--rebase-facts`).
///   `--first-install` replaces both, and is refused if either path given exists.
/// - `--write-facts` writes the new facts; `--stamp` writes a [`Stamp`], on success only.
pub fn check_command(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut o: BTreeMap<&str, String> = BTreeMap::new();
    let (mut first_install, mut rebase) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--first-install" => first_install = true,
            "--rebase-facts" => rebase = true,
            flag @ ("--result" | "--facts" | "--repo" | "--write-facts" | "--bin" | "--map"
            | "--library-bin" | "--stamp") => {
                let v = it.next().cloned().ok_or(format!("{flag} needs a value"))?;
                if o.insert(flag, v).is_some() {
                    return Err(format!("install-check: {flag} given twice"));
                }
            }
            other => return Err(format!("install-check: unknown argument `{other}`")),
        }
    }
    let usage = "usage: psoxide-test install-check --result RESULT.json --repo GAME_CLONE --bin \
                 BIN [--map MAP] (--facts FACTS.json --library-bin LIBRARY.bin | \
                 --first-install) [--rebase-facts] [--write-facts OUT.json] [--stamp OUT.json]";
    let (Some(result), Some(repo), Some(bin)) =
        (o.get("--result"), o.get("--repo"), o.get("--bin"))
    else {
        return Err(format!(
            "{usage}\n(--result, --repo and --bin are required)"
        ));
    };
    if !first_install && (!o.contains_key("--facts") || !o.contains_key("--library-bin")) {
        return Err(format!(
            "{usage}\n(--facts and --library-bin are required unless --first-install)"
        ));
    }
    if first_install && rebase {
        return Err("install-check: --first-install and --rebase-facts exclude each other".into());
    }
    let repo = Path::new(repo);
    let (hash, r) = read_result(Path::new(result))?;
    let game = game_at(repo, &r.commit)?;
    let refused = |e: String| -> Result<std::process::ExitCode, String> {
        println!("install-check REFUSED: {e}");
        Ok(std::process::ExitCode::FAILURE)
    };
    let bin_sha256 = util::sha256_file(Path::new(bin))?;
    let map_sha256 = match o.get("--map") {
        Some(m) => Some(util::sha256_file(Path::new(m))?),
        None => None,
    };
    if let Err(e) = judged_disc(&hash, &r, &game, &bin_sha256, map_sha256.as_deref()) {
        return refused(e);
    }
    let facts_path = o.get("--facts").map(Path::new);
    let library_bin = o.get("--library-bin").map(Path::new);
    let installed_facts = match facts_path {
        Some(p) => SpeedFacts::load(p)?,
        None => None,
    };
    let library_bin_sha = match library_bin {
        Some(p) if p.exists() => Some(util::sha256_file(p)?),
        _ => None,
    };
    let installed =
        if first_install {
            if let Some(p) = facts_path.filter(|_| installed_facts.is_some()) {
                return refused(format!(
                    "--first-install, but speed facts exist at {}: this is not a first install",
                    p.display()
                ));
            }
            if let (Some(p), Some(_)) = (library_bin, &library_bin_sha) {
                return refused(format!(
                "--first-install, but the library already holds {}: this is not a first install",
                p.display()
            ));
            }
            Installed::Nothing
        } else {
            match (&installed_facts, &library_bin_sha) {
                (Some(facts), Some(sha)) => Installed::Facts {
                    facts,
                    library_bin_sha256: sha,
                },
                (None, Some(_)) => Installed::DiscWithoutFacts,
                (Some(_), None) => {
                    return refused(format!(
                    "--library-bin {} does not exist but speed facts do: they describe nothing \
                     installed",
                    library_bin.map(Path::display).map_or(String::new(), |d| d.to_string())
                ))
                }
                (None, None) => {
                    return refused(format!(
                        "no speed facts at {} (and no library bin): if this game was never \
                     installed, say so with --first-install",
                        facts_path
                            .map(Path::display)
                            .map_or(String::new(), |d| d.to_string())
                    ))
                }
            }
        };
    let layouts = install_layouts(repo, &r.commit, &game)?;
    let identities = window_identities(repo, &r.commit, &game, &r.gate_revision)?;
    let judged = Judged {
        hash: &hash,
        result: &r,
        layouts: &layouts,
        identities: &identities,
        bin_sha256: &bin_sha256,
        map_sha256: map_sha256.as_deref(),
    };
    let accepted = match check(&judged, installed, rebase) {
        Ok(a) => a,
        Err(e) => return refused(e),
    };
    if let Some(out) = o.get("--write-facts") {
        std::fs::write(out, accepted.facts.to_json()?).map_err(|e| format!("write {out}: {e}"))?;
    }
    if let Some(out) = o.get("--stamp") {
        let stamp = Stamp {
            schema: STAMP_SCHEMA,
            result_sha256: hash.clone(),
            bin_sha256: bin_sha256.clone(),
            map_sha256: map_sha256.clone(),
            profile: r.profile.name().into(),
            checked_at: util::utc_now(),
            facts: accepted.facts.clone(),
        };
        let json = serde_json::to_string_pretty(&stamp).map_err(|e| e.to_string())?;
        let tmp = format!("{out}.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("write {tmp}: {e}"))?;
        std::fs::rename(&tmp, out).map_err(|e| format!("rename {tmp} to {out}: {e}"))?;
    }
    if accepted.rebased {
        println!("\n*** {} ***\n", accepted.note);
    }
    println!(
        "install-check PASS: result {} bin {}: {}",
        util::short(&hash),
        util::short(&bin_sha256),
        accepted.note
    );
    Ok(std::process::ExitCode::SUCCESS)
}

/// The rule the install stage applies to the bytes it installs, for files a script installs:
/// `bin_sha256` must be the bin an `[[install]]` build's disc the result's build stage recorded,
/// and `map_sha256` that build's recorded link map (given exactly when the build declares one).
fn judged_disc(
    hash: &str,
    r: &RunResult,
    game: &Game,
    bin_sha256: &str,
    map_sha256: Option<&str>,
) -> Result<(), String> {
    let at = util::short(hash);
    let recorded: BTreeMap<&str, &str> = r
        .stages
        .iter()
        .filter(|s| s.name == "build")
        .flat_map(|s| s.outcome.facts.iter())
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let build = game
        .installs
        .iter()
        .filter_map(|i| game.build(&i.build))
        .find(|b| {
            let key = format!("{}/{}", b.name, b.disc.replace(".cue", ".bin"));
            recorded.get(key.as_str()) == Some(&bin_sha256)
        })
        .ok_or_else(|| {
            format!(
                "--bin (sha256 {}) is not the bin of any [[install]] build that result {at} \
                 judged: install only the bytes the full tier judged",
                util::short(bin_sha256)
            )
        })?;
    match (&build.map, map_sha256) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(format!(
            "--map given, but build `{}` declares no map",
            build.name
        )),
        (Some(m), None) => Err(format!(
            "build `{}` declares the map `{m}`: give the map being installed with --map",
            build.name
        )),
        (Some(m), Some(sha)) => {
            let key = format!("{}/{m}", build.name);
            match recorded.get(key.as_str()) {
                Some(judged) if *judged == sha => Ok(()),
                Some(judged) => Err(format!(
                    "--map (sha256 {}) is not the map result {at} judged ({})",
                    util::short(sha),
                    util::short(judged)
                )),
                None => Err(format!(
                    "result {at} records no `{key}`: it cannot vouch for the map being installed"
                )),
            }
        }
    }
}

/// The game file as committed at `commit` in `repo`.
fn game_at(repo: &Path, commit: &str) -> Result<Game, String> {
    let file = format!("{commit}:{}", crate::game::GAME_FILE);
    let text = util::git(repo, &["show", &file]).map_err(|e| {
        format!(
            "--repo {}: no game file at {}: {e}",
            repo.display(),
            util::short(commit)
        )
    })?;
    Game::parse(&text).map_err(|e| format!("{file}: {e}"))
}

/// The committed layouts of every `[[install]]` build of `game` at `commit` in `repo`.
fn install_layouts(repo: &Path, commit: &str, game: &Game) -> Result<Vec<CommittedLayout>, String> {
    let file = format!("{}:{}", util::short(commit), crate::game::GAME_FILE);
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

/// A result file by path: its content hash and contents. The file must be named
/// `<sha256 of its content>.json`, as `run` writes it: anything else (an edited result, or a
/// copy under another name) is refused.
fn read_result(path: &Path) -> Result<(String, RunResult), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let hash = util::sha256_bytes(text.as_bytes());
    let named = path.extension().and_then(|e| e.to_str()) == Some("json")
        && path.file_stem().and_then(|s| s.to_str()) == Some(hash.as_str());
    if !named {
        return Err(format!(
            "{} is not named after its content (sha256 {hash}.json): only an unedited result \
             file, as `psoxide-test run` wrote it, can be installed",
            path.display()
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
    use std::process::ExitCode;

    const BIN: &[u8] = b"the judged bin";
    const MAP: &[u8] = b"the judged map";

    /// A complete clean full install-profile PASS whose one window `w` measured `work` cycles
    /// and `flips` frames at offsets -12, 0 and 12 (p99 = work + 10), and whose build stage
    /// recorded build `b`'s `g.bin` and `g.map` as [`BIN`] and [`MAP`].
    fn passing(work: [u64; 3], flips: [u64; 3]) -> RunResult {
        let mut facts = BTreeMap::new();
        for (i, offset) in [-12, 0, 12].into_iter().enumerate() {
            facts.insert(format!("w/{offset}/work_per_frame"), work[i].to_string());
            facts.insert(
                format!("w/{offset}/frame_work_p99"),
                (work[i] + 10).to_string(),
            );
            facts.insert(format!("w/{offset}/flips"), flips[i].to_string());
            facts.insert(format!("w/{offset}/vblanks"), "1:700".to_string());
        }
        facts.insert("w/polls".to_string(), "10..20".to_string());
        let stage = |name: &str, facts| StageResult {
            name: name.into(),
            source: Source::Run,
            key: String::new(),
            outcome: Outcome {
                checks: vec![Check::pass(name, "")],
                facts,
            },
        };
        let built = BTreeMap::from([
            ("b/g.bin".to_string(), util::sha256_bytes(BIN)),
            ("b/g.cue".to_string(), util::sha256_bytes(b"cue")),
            ("b/g.map".to_string(), util::sha256_bytes(MAP)),
        ]);
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
            stages: vec![stage("build", built), stage("speed", facts)],
            skips: Vec::new(),
            verdict: Status::Pass,
            landing_missing: Vec::new(),
        };
        r.finish();
        r
    }

    /// The identity of row `w` (polls 10..20, tape `t.tape` with sha256 `tape`).
    fn identity(tape: &str) -> WindowIdentity {
        WindowIdentity {
            tape: Some("t.tape".into()),
            tape_sha256: Some(tape.into()),
            window: "polls 10..20".into(),
            offsets: vec![-12, 0, 12],
            launch_args: Vec::new(),
            gate_revision: "r".repeat(40),
        }
    }

    fn ids(tape: &str) -> BTreeMap<String, WindowIdentity> {
        BTreeMap::from([("w".to_string(), identity(tape))])
    }

    /// [`check`] of `r` (hash `hash`, bin "bin-<hash>"), no layouts, row `w` on tape `tape`.
    fn judge(
        hash: &str,
        r: &RunResult,
        tape: &str,
        installed: Installed<'_>,
        rebase: bool,
    ) -> Result<Accepted, String> {
        let identities = ids(tape);
        let bin = format!("bin-{hash}");
        let j = Judged {
            hash,
            result: r,
            layouts: &[],
            identities: &identities,
            bin_sha256: &bin,
            map_sha256: Some("map"),
        };
        check(&j, installed, rebase)
    }

    /// The facts of a first install of `r` under hash `hash`.
    fn first(hash: &str, r: &RunResult) -> SpeedFacts {
        judge(hash, r, "t1", Installed::Nothing, false)
            .unwrap()
            .facts
    }

    /// The library holding `facts`' own bin.
    fn over(facts: &SpeedFacts) -> Installed<'_> {
        Installed::Facts {
            facts,
            library_bin_sha256: &facts.bin_sha256,
        }
    }

    #[test]
    fn the_first_install_is_accepted_and_its_facts_bound_to_the_disc() {
        let r = passing([100, 102, 104], [790, 786, 782]);
        let a = judge("ab", &r, "t1", Installed::Nothing, false).unwrap();
        assert!(a.note.contains("first install") && !a.rebased, "{}", a.note);
        let facts = a.facts;
        assert_eq!(facts.schema, FACTS_SCHEMA);
        assert_eq!(facts.result_sha256, "ab");
        assert_eq!(facts.bin_sha256, "bin-ab");
        assert_eq!(facts.map_sha256.as_deref(), Some("map"));
        assert_eq!(
            facts.windows["w"],
            FactsWindow {
                offsets: vec![-12, 0, 12],
                flips: vec![790, 786, 782],
                work_per_frame: vec![100, 102, 104],
                frame_work_p99: vec![110, 112, 114],
                identity: Some(identity("t1")),
            }
        );
        // Written and read back through the sidecar format; a missing file reads as none.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("G{FACTS_SUFFIX}"));
        assert_eq!(SpeedFacts::load(&path).unwrap(), None);
        std::fs::write(&path, facts.to_json().unwrap()).unwrap();
        assert_eq!(SpeedFacts::load(&path).unwrap(), Some(facts.clone()));
        let mut future = facts;
        future.schema = FACTS_SCHEMA + 1;
        std::fs::write(&path, future.to_json().unwrap()).unwrap();
        assert!(SpeedFacts::load(&path).unwrap_err().contains("not one"));
    }

    #[test]
    fn a_landing_profile_or_newer_schema_result_is_refused() {
        let mut r = passing([100, 102, 104], [790, 786, 782]);
        r.profile = Profile::Landing;
        r.schema = crate::report::result_schema(Profile::Landing);
        let e = judge("ab", &r, "t1", Installed::Nothing, false).unwrap_err();
        assert!(
            e.contains("landing-profile PASS, not an install pass"),
            "{e}"
        );
        let mut r = passing([100, 102, 104], [790, 786, 782]);
        r.schema = RESULT_SCHEMA_MAX + 1;
        let e = judge("ab", &r, "t1", Installed::Nothing, false).unwrap_err();
        assert!(e.contains("newer than this psoxide-test reads"), "{e}");
    }

    #[test]
    fn an_incomplete_or_dirty_or_failed_result_is_refused() {
        let base = passing([100, 102, 104], [790, 786, 782]);
        let refused = |r: &RunResult| judge("ab", r, "t1", Installed::Nothing, false).unwrap_err();
        let mut r = base.clone();
        r.landing_missing = vec!["tier full".into()];
        assert!(refused(&r).contains("complete full PASS"));
        let mut r = base.clone();
        r.verdict = Status::Fail;
        assert!(refused(&r).contains("complete full PASS"));
        let mut r = base;
        r.dirty = true;
        assert!(refused(&r).contains("dirty"));
    }

    #[test]
    fn slower_than_the_installed_build_is_refused() {
        let installed = first("old", &passing([100, 102, 104], [790, 786, 782]));
        let new = |r: &RunResult| judge("new", r, "t1", over(&installed), false);
        // Spread over three offsets: work (104-100)/3, flips (790-782)/3. Inside it passes.
        let same = passing([101, 103, 105], [789, 785, 781]);
        let a = new(&same).unwrap();
        assert!(a.note.contains("not slower") && !a.rebased, "{}", a.note);
        assert!(new(&passing([90, 91, 92], [795, 790, 788])).is_ok());
        let e = new(&passing([103, 105, 107], [790, 786, 782])).unwrap_err();
        assert!(e.contains("window `w` work per frame is 105.0"), "{e}");
        assert!(e.contains("had 102.0"), "{e}");
        let e = new(&passing([100, 102, 104], [786, 782, 778])).unwrap_err();
        assert!(e.contains("window `w` frames presented is 782.0"), "{e}");
        assert!(e.contains("had 786.0"), "{e}");
        // A window the installed build was judged on must be measured again.
        let mut gone = passing([100, 102, 104], [790, 786, 782]);
        gone.stages.retain(|s| s.name != "speed");
        assert!(new(&gone).unwrap_err().contains("has no numbers for it"));
        // --rebase-facts never waives a comparison that can be made.
        let slower = passing([110, 112, 114], [790, 786, 782]);
        assert!(judge("new", &slower, "t1", over(&installed), true).is_err());
    }

    #[test]
    fn a_worse_p99_beyond_the_installed_spread_is_refused() {
        let installed = first("old", &passing([100, 102, 104], [790, 786, 782]));
        // Same mean work, same flips, but a p99 30 cycles worse at every offset.
        let mut r = passing([100, 102, 104], [790, 786, 782]);
        for (k, v) in r.stages[1].outcome.facts.iter_mut() {
            if k.ends_with("/frame_work_p99") {
                *v = (v.parse::<u64>().unwrap() + 30).to_string();
            }
        }
        let e = judge("new", &r, "t1", over(&installed), false).unwrap_err();
        assert!(e.contains("work per frame p99 is 142.0"), "{e}");
        // Within the installed spread ((114-110)/3) it passes.
        for (k, v) in r.stages[1].outcome.facts.iter_mut() {
            if k.ends_with("/frame_work_p99") {
                *v = (v.parse::<u64>().unwrap() - 29).to_string();
            }
        }
        assert!(judge("new", &r, "t1", over(&installed), false).is_ok());
        // A p99 the installed build had must be measured again.
        r.stages[1]
            .outcome
            .facts
            .retain(|k, _| !k.ends_with("/frame_work_p99"));
        let e = judge("new", &r, "t1", over(&installed), false).unwrap_err();
        assert!(e.contains("has a p99"), "{e}");
    }

    #[test]
    fn a_changed_window_identity_is_refused_unless_rebased() {
        let installed = first("old", &passing([100, 102, 104], [790, 786, 782]));
        // Much slower, but on another tape: not comparable, so refused as such, not as slower.
        let slower = passing([200, 202, 204], [790, 786, 782]);
        let e = judge("new", &slower, "t2", over(&installed), false).unwrap_err();
        assert!(e.contains("measure something else"), "{e}");
        assert!(e.contains("tape sha256 t1 -> t2"), "{e}");
        assert!(e.contains("--rebase-facts"), "{e}");
        let a = judge("new", &slower, "t2", over(&installed), true).unwrap();
        assert!(a.rebased);
        assert!(a.note.starts_with("REBASED SPEED REFERENCE"), "{}", a.note);
        assert!(a.note.contains("NOT judged"), "{}", a.note);
        assert_eq!(a.facts.windows["w"].identity, Some(identity("t2")));
        // A window no longer declared is a changed identity too.
        let identities = BTreeMap::new();
        let j = Judged {
            hash: "new",
            result: &slower,
            layouts: &[],
            identities: &identities,
            bin_sha256: "bin-new",
            map_sha256: None,
        };
        let e = check(&j, over(&installed), false).unwrap_err();
        assert!(e.contains("no longer declared"), "{e}");
        // Every field of the identity counts.
        let a = identity("t1");
        for (field, b) in [
            (
                "window",
                WindowIdentity {
                    window: "polls 11..21".into(),
                    ..a.clone()
                },
            ),
            (
                "offsets",
                WindowIdentity {
                    offsets: vec![0],
                    ..a.clone()
                },
            ),
            (
                "launch args",
                WindowIdentity {
                    launch_args: vec!["x".into()],
                    ..a.clone()
                },
            ),
            (
                "gate revision",
                WindowIdentity {
                    gate_revision: "q".repeat(40),
                    ..a.clone()
                },
            ),
            (
                "tape",
                WindowIdentity {
                    tape: None,
                    ..a.clone()
                },
            ),
        ] {
            let d = a.differences(&b);
            assert!(d.len() == 1 && d[0].starts_with(field), "{field}: {d:?}");
        }
    }

    #[test]
    fn facts_must_describe_the_installed_disc() {
        let installed = first("old", &passing([100, 102, 104], [790, 786, 782]));
        let r = passing([100, 102, 104], [790, 786, 782]);
        let elsewhere = Installed::Facts {
            facts: &installed,
            library_bin_sha256: "another-bin",
        };
        let e = judge("new", &r, "t1", elsewhere, false).unwrap_err();
        assert!(e.contains("do not describe what is installed"), "{e}");
        assert!(judge("new", &r, "t1", elsewhere, true).unwrap().rebased);
        // A disc without facts, and schema-1 facts, cannot be compared either.
        let e = judge("new", &r, "t1", Installed::DiscWithoutFacts, false).unwrap_err();
        assert!(e.contains("no speed facts describe it"), "{e}");
        assert!(
            judge("new", &r, "t1", Installed::DiscWithoutFacts, true)
                .unwrap()
                .rebased
        );
        let legacy = SpeedFacts {
            schema: 1,
            bin_sha256: String::new(),
            map_sha256: None,
            ..installed.clone()
        };
        let e = judge("new", &r, "t1", over(&legacy), false).unwrap_err();
        assert!(e.contains("schema 1"), "{e}");
    }

    #[test]
    fn the_install_stage_reads_the_library_strictly() {
        let facts = first("old", &passing([100, 102, 104], [790, 786, 782]));
        assert!(matches!(
            library_installed(None, None, false),
            Ok(Installed::Nothing)
        ));
        // A disc in the library and no facts is never a first install.
        assert!(matches!(
            library_installed(None, Some("x"), true),
            Ok(Installed::DiscWithoutFacts)
        ));
        assert!(matches!(
            library_installed(None, None, true),
            Ok(Installed::DiscWithoutFacts)
        ));
        assert!(matches!(
            library_installed(Some(&facts), Some("x"), true),
            Ok(Installed::Facts {
                library_bin_sha256: "x",
                ..
            })
        ));
        assert!(library_installed(Some(&facts), None, false)
            .unwrap_err()
            .contains("describe nothing installed"));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(library_state(dir.path(), "G").unwrap(), (None, None, false));
        std::fs::write(dir.path().join("G.cue"), "cue").unwrap();
        assert_eq!(library_state(dir.path(), "G").unwrap(), (None, None, true));
        std::fs::write(dir.path().join("G.bin"), BIN).unwrap();
        std::fs::write(
            dir.path().join(format!("G{FACTS_SUFFIX}")),
            facts.to_json().unwrap(),
        )
        .unwrap();
        assert_eq!(
            library_state(dir.path(), "G").unwrap(),
            (Some(facts), Some(util::sha256_bytes(BIN)), true)
        );
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
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
    }

    /// Write `files` into `repo` and commit them all; returns the commit.
    fn commit_files(repo: &Path, files: &[(&str, &str)]) -> String {
        for (path, text) in files {
            let path = repo.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-q", "-m", "c"]);
        util::git(repo, &["rev-parse", "HEAD"]).unwrap()
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
        git_in(&repo, &["init", "-q"]);
        let commit = commit_files(&repo, files);
        (repo, commit)
    }

    const GREEDY: &str = "# psoxide-pgo layout 1\n# features: (default)\n# variant: profi\n\
                          f 0000000000000001 1 a\nw 0:5\n";
    const SEARCHED: &str = "# psoxide-pgo layout 1\n# psoxide-pgo searched: rounds 600 seed 1\n\
                            # features: (default)\n# variant: profi\nf 0000000000000001 1 a\n\
                            w 0:5\n# placement: psoxide-pgo search, seed 1 rounds 600\n\
                            s 80010000\no 0000000000000001 1 80010000 a\n";

    /// The `[[build]]` tail of a game with a map and one speed window `w` on `t.tape`.
    const MAPPED_WINDOW: &str = "map = \"g.map\"\n[[speed]]\nname = \"w\"\nbuild = \"b\"\n\
                                 tape = \"t.tape\"\npolls = [10, 20]\noffsets = [-12, 0, 12]\n";

    #[test]
    fn the_search_stamp_is_read_from_the_header_only() {
        assert!(search_stamped(SEARCHED));
        assert_eq!(search_stamp(SEARCHED), Some((600, 1)));
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
        let identities = ids("t1");
        let with = |layouts: &[CommittedLayout]| {
            let j = Judged {
                hash: "ab",
                result: &r,
                layouts,
                identities: &identities,
                bin_sha256: "bin",
                map_sha256: None,
            };
            check(&j, Installed::Nothing, false)
        };
        // No layout at all: nothing to search.
        assert!(with(&[]).is_ok());
        assert!(with(&[layout("pgo/g.layout", SEARCHED)]).is_ok());
        let e = with(&[
            layout("pgo/g.layout", SEARCHED),
            layout("pgo/t.layout", GREEDY),
        ])
        .unwrap_err();
        assert!(
            e.contains("did not write") && e.contains("pgo/t.layout"),
            "{e}"
        );
        assert!(!e.contains("pgo/g.layout"), "{e}");
        assert!(e.contains("psoxide-pgo search"), "{e}");
        // A stamp from a search stopped early is refused, naming its rounds.
        let one = SEARCHED.replacen("rounds 600 seed 1", "rounds 1 seed 1", 1);
        assert_ne!(one, SEARCHED);
        let e = with(&[layout("pgo/g.layout", &one)]).unwrap_err();
        assert!(
            e.contains("fewer than 600") && e.contains("rounds 1 seed 1"),
            "{e}"
        );
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
    fn window_identities_are_read_at_the_judged_commit() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, one) = game_repo(dir.path(), MAPPED_WINDOW, &[("t.tape", "tape one")]);
        let two = commit_files(&repo, &[("t.tape", "tape two")]);
        let game = game_at(&repo, &one).unwrap();
        let a = window_identities(&repo, &one, &game, "gate").unwrap();
        let b = window_identities(&repo, &two, &game, "gate").unwrap();
        assert_eq!(
            a["w"],
            WindowIdentity {
                tape: Some("t.tape".into()),
                tape_sha256: Some(util::sha256_bytes(b"tape one")),
                window: "polls 10..20".into(),
                offsets: vec![-12, 0, 12],
                launch_args: Vec::new(),
                gate_revision: "gate".into(),
            }
        );
        assert_eq!(
            a["w"].differences(&b["w"]),
            [format!(
                "tape sha256 {} -> {}",
                util::short(&util::sha256_bytes(b"tape one")),
                util::short(&util::sha256_bytes(b"tape two"))
            )]
        );
    }

    #[test]
    fn a_result_not_named_after_its_content_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::to_string_pretty(&passing([1, 2, 3], [4, 5, 6])).unwrap();
        let hash = util::sha256_bytes(json.as_bytes());
        let named = dir.path().join(format!("{hash}.json"));
        std::fs::write(&named, &json).unwrap();
        assert_eq!(read_result(&named).unwrap().0, hash);
        // The same bytes under another name, and edited bytes under the old name.
        let other = dir.path().join("r.json");
        std::fs::write(&other, &json).unwrap();
        assert!(read_result(&other)
            .unwrap_err()
            .contains("not named after its content"));
        std::fs::write(&named, json.replace("\"full\"", "\"quick\"")).unwrap();
        assert!(read_result(&named).is_err());
    }

    /// A game clone with a map and window `w`, a scratch library, and an `install-check`
    /// argument builder.
    struct Bench {
        dir: tempfile::TempDir,
        repo: std::path::PathBuf,
        commit: String,
    }

    impl Bench {
        fn new() -> Bench {
            let dir = tempfile::tempdir().unwrap();
            let (repo, commit) = game_repo(
                dir.path(),
                MAPPED_WINDOW,
                &[("pgo/g.layout", SEARCHED), ("t.tape", "tape")],
            );
            std::fs::write(dir.path().join("g.bin"), BIN).unwrap();
            std::fs::write(dir.path().join("g.map"), MAP).unwrap();
            Bench { dir, repo, commit }
        }

        fn path(&self, name: &str) -> String {
            self.dir.path().join(name).display().to_string()
        }

        /// Save `r` at this commit as a hash-named result file.
        fn result(&self, r: &RunResult) -> String {
            let mut r = r.clone();
            r.commit = self.commit.clone();
            let json = serde_json::to_string_pretty(&r).unwrap();
            let path = self.path(&format!("{}.json", util::sha256_bytes(json.as_bytes())));
            std::fs::write(&path, &json).unwrap();
            path
        }

        /// The normal install line, with `extra` appended.
        fn args(&self, result: &str, extra: &[&str]) -> Vec<String> {
            let mut a: Vec<String> = [
                "--result",
                result,
                "--repo",
                &self.repo.display().to_string(),
                "--bin",
                &self.path("g.bin"),
                "--map",
                &self.path("g.map"),
                "--facts",
                &self.path("lib/G.speed-facts.json"),
                "--library-bin",
                &self.path("lib/G.bin"),
                "--write-facts",
                &self.path("new-facts.json"),
                "--stamp",
                &self.path("stamp.json"),
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            a.extend(extra.iter().map(|s| s.to_string()));
            a
        }

        /// The first-install line: no --facts and --library-bin.
        fn first_args(&self, result: &str) -> Vec<String> {
            let mut a = self.args(result, &["--first-install"]);
            let at = a.iter().position(|s| s == "--facts").unwrap();
            a.drain(at..at + 4);
            a
        }

        /// Deploy as a script would: the bin and the new facts into the library.
        fn deploy(&self) {
            std::fs::create_dir_all(self.path("lib")).unwrap();
            std::fs::copy(self.path("g.bin"), self.path("lib/G.bin")).unwrap();
            std::fs::rename(
                self.path("new-facts.json"),
                self.path("lib/G.speed-facts.json"),
            )
            .unwrap();
            std::fs::remove_file(self.path("stamp.json")).unwrap();
        }

        fn stamp(&self) -> Option<Stamp> {
            let text = std::fs::read_to_string(self.path("stamp.json")).ok()?;
            Some(serde_json::from_str(&text).unwrap())
        }
    }

    #[test]
    fn install_check_needs_explicit_first_install() {
        let b = Bench::new();
        let r = b.result(&passing([100, 102, 104], [790, 786, 782]));
        // Facts missing and no library disc: refused without --first-install (a typo in
        // --facts must not disable never-slower).
        assert_eq!(check_command(&b.args(&r, &[])).unwrap(), ExitCode::FAILURE);
        assert!(b.stamp().is_none());
        // --facts and --library-bin are required without it.
        let mut no_facts = b.args(&r, &[]);
        no_facts.drain(8..12);
        assert!(check_command(&no_facts)
            .unwrap_err()
            .contains("--first-install"));
        assert_eq!(check_command(&b.first_args(&r)).unwrap(), ExitCode::SUCCESS);
        b.deploy();
        // Now the library holds a disc and facts: --first-install is refused.
        let with_paths = b.args(&r, &["--first-install"]);
        assert_eq!(check_command(&with_paths).unwrap(), ExitCode::FAILURE);
        // A library disc without facts is refused, and --rebase-facts takes it.
        std::fs::remove_file(b.path("lib/G.speed-facts.json")).unwrap();
        assert_eq!(check_command(&b.args(&r, &[])).unwrap(), ExitCode::FAILURE);
        assert_eq!(
            check_command(&b.args(&r, &["--rebase-facts"])).unwrap(),
            ExitCode::SUCCESS
        );
        assert!(check_command(&b.args(&r, &["--first-install", "--rebase-facts"])).is_err());
    }

    #[test]
    fn install_check_compares_with_the_installed_disc_and_writes_a_stamp() {
        let b = Bench::new();
        let r = b.result(&passing([100, 102, 104], [790, 786, 782]));
        assert_eq!(check_command(&b.first_args(&r)).unwrap(), ExitCode::SUCCESS);
        let stamp = b.stamp().unwrap();
        let hash = Path::new(&r).file_stem().unwrap().to_str().unwrap();
        assert_eq!(stamp.schema, STAMP_SCHEMA);
        assert_eq!(stamp.result_sha256, hash);
        assert_eq!(stamp.bin_sha256, util::sha256_bytes(BIN));
        assert_eq!(stamp.map_sha256, Some(util::sha256_bytes(MAP)));
        assert_eq!(stamp.profile, "install");
        assert_eq!(stamp.checked_at.len(), "2026-11-22T12:34:56Z".len());
        let written: SpeedFacts =
            serde_json::from_str(&std::fs::read_to_string(b.path("new-facts.json")).unwrap())
                .unwrap();
        assert_eq!(stamp.facts, written);
        assert_eq!(written.bin_sha256, stamp.bin_sha256);
        assert!(written.windows["w"].identity.is_some());
        b.deploy();
        // Not slower: passes against the installed facts.
        let same = b.result(&passing([101, 103, 105], [789, 785, 781]));
        assert_eq!(
            check_command(&b.args(&same, &[])).unwrap(),
            ExitCode::SUCCESS
        );
        std::fs::remove_file(b.path("stamp.json")).unwrap();
        // Slower: refused, and no stamp.
        let slower = b.result(&passing([110, 112, 114], [790, 786, 782]));
        assert_eq!(
            check_command(&b.args(&slower, &[])).unwrap(),
            ExitCode::FAILURE
        );
        assert!(b.stamp().is_none());
        // The library's bin replaced behind the facts' back: they no longer describe it.
        std::fs::write(b.path("lib/G.bin"), "another disc").unwrap();
        assert_eq!(
            check_command(&b.args(&same, &[])).unwrap(),
            ExitCode::FAILURE
        );
        assert!(b.stamp().is_none());
        // A landing result is refused.
        let mut landing = passing([100, 102, 104], [790, 786, 782]);
        landing.profile = Profile::Landing;
        landing.schema = 2;
        let landing = b.result(&landing);
        assert_eq!(
            check_command(&b.first_args(&landing)).unwrap(),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn install_check_refuses_a_bin_or_map_the_result_did_not_judge() {
        let b = Bench::new();
        let r = b.result(&passing([100, 102, 104], [790, 786, 782]));
        let with = |file: &str, bytes: &[u8]| {
            std::fs::write(b.path(file), bytes).unwrap();
            let code = check_command(&b.first_args(&r)).unwrap();
            std::fs::write(b.path("g.bin"), BIN).unwrap();
            std::fs::write(b.path("g.map"), MAP).unwrap();
            code
        };
        assert_eq!(with("g.bin", BIN), ExitCode::SUCCESS);
        assert_eq!(with("g.bin", b"another bin"), ExitCode::FAILURE);
        assert_eq!(with("g.map", b"another map"), ExitCode::FAILURE);
        // The build declares a map: --map is required.
        let mut no_map = b.first_args(&r);
        let at = no_map.iter().position(|s| s == "--map").unwrap();
        no_map.drain(at..at + 2);
        assert_eq!(check_command(&no_map).unwrap(), ExitCode::FAILURE);
        // --repo and --bin are required.
        let mut no_repo = b.first_args(&r);
        no_repo.drain(2..4);
        assert!(check_command(&no_repo).unwrap_err().contains("--repo"));
        // A repo without the result's commit is an error, never a pass.
        let other = tempfile::tempdir().unwrap();
        let (other_repo, _) = game_repo(other.path(), "", &[]);
        let mut elsewhere = b.first_args(&r);
        elsewhere[3] = other_repo.display().to_string();
        assert!(check_command(&elsewhere).is_err());
    }

    #[test]
    fn install_check_refuses_a_greedy_layout() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, commit) = game_repo(
            dir.path(),
            MAPPED_WINDOW,
            &[("pgo/g.layout", GREEDY), ("t.tape", "tape")],
        );
        let b = Bench { dir, repo, commit };
        std::fs::write(b.path("g.bin"), BIN).unwrap();
        std::fs::write(b.path("g.map"), MAP).unwrap();
        let r = b.result(&passing([100, 102, 104], [790, 786, 782]));
        assert_eq!(check_command(&b.first_args(&r)).unwrap(), ExitCode::FAILURE);
    }
}
