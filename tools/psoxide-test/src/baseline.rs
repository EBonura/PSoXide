//! The baseline, `psoxide-test.baseline.toml`: the numbers a run is judged against. Written only
//! by `psoxide-test bless`, committed in the commit that lands a build.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::game::BASELINE_FILE;
use crate::stage::install::WindowIdentity;

/// The whole baseline.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema: u32,
    /// The commit the numbers were measured on.
    pub blessed_commit: String,
    /// The SDK revision that commit pins.
    pub sdk_pin: String,
    /// One link's size per `[[size]]` build, by build name.
    #[serde(default)]
    pub size: BTreeMap<String, SizeBase>,
    /// One table per speed window.
    #[serde(default)]
    pub speed: BTreeMap<String, WindowBase>,
    /// Per journey: ticks on `real` (the schedule) and the open-loop ratchet.
    #[serde(default)]
    pub journeys: BTreeMap<String, JourneyBase>,
    /// Host-check metrics, by check name (`name` or `name@model`).
    #[serde(default)]
    pub host: BTreeMap<String, u64>,
    /// Per `[[size]]` build: the SHA-256 of the blessed link map, kept by `bless` in the work
    /// tree (`<work>/<game>/maps/<sha256>.map`) so a size change can be explained by function.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub maps: BTreeMap<String, String>,
    /// Deepest stack in bytes by journey (`[[stack]]`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stack: BTreeMap<String, u32>,
    /// Load time by `[[load]]` row.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub load: BTreeMap<String, crate::stage::load::LoadBase>,
}

/// One link's size, from `psoxide_size::ram`.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SizeBase {
    pub text: u32,
    pub data: u32,
    pub bss: u32,
    /// Free RAM; absent when the map names no heap (a program with its own allocator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<u32>,
}

/// One window's metrics, one entry per offset.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowBase {
    pub offsets: Vec<i32>,
    pub flips: Vec<u64>,
    pub work_per_frame: Vec<u64>,
    pub frame_work_p99: Vec<u64>,
    pub vblanks: Vec<String>,
    /// The layout profiles the row's build linked when a plain bless measured it, read at the
    /// blessed commit; empty when the build links no layout. Absent in baselines blessed before
    /// it was recorded. `bless --keep-speed` carries it unchanged with the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layouts: Option<Vec<LayoutRecord>>,
    /// What the row measured (the install facts' identity), when a plain bless measured it.
    /// Absent in baselines blessed before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<WindowIdentity>,
    /// Set by `bless --keep-speed --trust-legacy-rows`: these rows were blessed before layout
    /// provenance was recorded and are trusted as the LANDING reference only (never by install,
    /// which judges the layout stamp and the library's facts on its own). Names the commit the
    /// rows were blessed on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_trusted: Option<String>,
}

/// One layout profile a speed row's build linked when it was blessed.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LayoutRecord {
    /// Its path in the repository.
    pub path: String,
    /// The SHA-256 of its bytes at the blessed commit.
    pub sha256: String,
    /// psoxide-pgo's search stamp (`# psoxide-pgo searched: rounds N seed N`); `None`: greedy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searched: Option<SearchStamp>,
}

/// The search stamp of a layout profile.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchStamp {
    pub rounds: u64,
    pub seed: u64,
}

/// One journey's schedule hint and open-loop count.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JourneyBase {
    pub ticks: u64,
    pub open_loop: u32,
    /// Waits and holds until a free-running clock reaches an absolute value (a fixed wait under
    /// another name). Absent in baselines blessed before it was counted: then not enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_waits: Option<u32>,
}

impl Baseline {
    /// Read the baseline beside the game file; `None` when there is none yet.
    pub fn load(repo: &Path) -> Result<Option<Baseline>, String> {
        let path = repo.join(BASELINE_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let base: Baseline =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if base.schema != 1 {
            return Err(format!(
                "{}: schema {} is not 1",
                path.display(),
                base.schema
            ));
        }
        Ok(Some(base))
    }

    /// Write the baseline beside the game file.
    pub fn save(&self, repo: &Path) -> Result<(), String> {
        let path = repo.join(BASELINE_FILE);
        let body = toml::to_string(self).map_err(|e| format!("encode baseline: {e}"))?;
        let text = format!(
            "# Written by `psoxide-test bless`; edit only by blessing again (decision 4: a bar \
             changes by editing\n# psoxide-test.toml with a reason, never by hand-editing these \
             numbers).\n{body}"
        );
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// `bless --keep-speed`: the speed rows of `old` to keep, refused unless every row was measured
/// by a plain bless on searched layouts (or no layout) and still measures what the game file and
/// config declare now (`now`: every declared row's current identity).
pub fn kept_speed(
    old: &Baseline,
    now: &BTreeMap<String, WindowIdentity>,
    trust_legacy: bool,
) -> Result<BTreeMap<String, WindowBase>, String> {
    let missing: Vec<&str> = now
        .keys()
        .filter(|row| !old.speed.contains_key(*row))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = old
        .speed
        .keys()
        .filter(|row| !now.contains_key(*row))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(format!(
            "--keep-speed keeps every speed row or none: the game file declares rows the \
             baseline lacks [{}] and the baseline has rows the game file no longer declares \
             [{}]. Run a plain `psoxide-test bless` on a searched layout",
            missing.join(", "),
            extra.join(", ")
        ));
    }
    // `--trust-legacy-rows`: only a baseline none of whose rows carries a record (every row
    // blessed before records existed) is adopted, each row taking today's identity and the mark.
    let none_recorded = old
        .speed
        .values()
        .all(|w| w.layouts.is_none() && w.identity.is_none() && w.legacy_trusted.is_none());
    if trust_legacy {
        if !none_recorded || old.speed.is_empty() {
            return Err(
                "--trust-legacy-rows adopts only a baseline whose speed rows all predate \
                 provenance records; this one has records: drop the flag"
                    .into(),
            );
        }
        let mut rows = old.speed.clone();
        for (row, w) in rows.iter_mut() {
            w.identity = now.get(row).cloned();
            w.legacy_trusted = Some(old.blessed_commit.clone());
        }
        return Ok(rows);
    }
    let unrecorded: Vec<&str> = old
        .speed
        .iter()
        .filter(|(_, w)| {
            w.identity.is_none() || (w.layouts.is_none() && w.legacy_trusted.is_none())
        })
        .map(|(row, _)| row.as_str())
        .collect();
    if !unrecorded.is_empty() {
        return Err(format!(
            "--keep-speed: the baseline records no layout provenance or window identity for \
             [{}] (blessed before they were recorded). Run one plain `psoxide-test bless` on a \
             layout `psoxide-pgo search` wrote, commit it, then --keep-speed may keep its rows \
             (or, for a landing only, adopt the rows once with --trust-legacy-rows)",
            unrecorded.join(", ")
        ));
    }
    let greedy: Vec<String> = old
        .speed
        .iter()
        .flat_map(|(row, w)| {
            w.layouts
                .iter()
                .flatten()
                .filter(|l| l.searched.is_none())
                .map(move |l| format!("`{row}`: {}", l.path))
        })
        .collect();
    if !greedy.is_empty() {
        return Err(format!(
            "--keep-speed keeps only rows measured on a searched layout; the baseline's rows \
             were blessed on layouts without the search stamp: {}. Run the layout search, \
             commit the layout, and run a plain `psoxide-test bless`",
            greedy.join(", ")
        ));
    }
    let moved: Vec<String> = old
        .speed
        .iter()
        .filter_map(|(row, w)| {
            let (was, is) = (w.identity.as_ref()?, now.get(row)?);
            (was != is).then(|| format!("`{row}` ({})", was.differences(is).join(", ")))
        })
        .collect();
    if !moved.is_empty() {
        return Err(format!(
            "--keep-speed: the kept rows no longer measure what the game file and config \
             declare: {}. Run a plain `psoxide-test bless` on a searched layout",
            moved.join("; ")
        ));
    }
    Ok(old.speed.clone())
}

impl WindowBase {
    /// Tolerance of one metric: the range over the blessed offsets divided by their number,
    /// the mean step between adjacent window phases.
    pub fn tolerance(values: &[u64]) -> f64 {
        match (values.iter().min(), values.iter().max()) {
            (Some(lo), Some(hi)) if !values.is_empty() => (hi - lo) as f64 / values.len() as f64,
            _ => 0.0,
        }
    }

    /// The blessed value at `offset`.
    pub fn at(&self, values: &[u64], offset: i32) -> Option<u64> {
        let index = self.offsets.iter().position(|o| *o == offset)?;
        values.get(index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_is_the_mean_phase_step() {
        let flips = [791, 791, 787, 785, 779, 779, 779, 779];
        assert!((WindowBase::tolerance(&flips) - 1.5).abs() < 1e-9);
        assert_eq!(WindowBase::tolerance(&[5]), 0.0);
    }

    #[test]
    fn a_baseline_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = Baseline {
            schema: 1,
            blessed_commit: "abc".into(),
            sdk_pin: "def".into(),
            ..Baseline::default()
        };
        base.size.insert(
            "ship".into(),
            SizeBase {
                text: 1,
                data: 2,
                bss: 3,
                free: Some(4),
            },
        );
        base.journeys.insert(
            "journey".into(),
            JourneyBase {
                ticks: 1822,
                open_loop: 3,
                clock_waits: Some(1),
            },
        );
        base.save(dir.path()).unwrap();
        let back = Baseline::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.size, base.size);
        assert_eq!(back.journeys["journey"].ticks, 1822);
    }

    /// Row `w`'s identity: tape `t.tape` with sha256 `tape`, offsets -12, 0, 12.
    fn identity(tape: &str) -> WindowIdentity {
        WindowIdentity {
            tape: Some("t.tape".into()),
            tape_sha256: Some(tape.into()),
            window: "polls 10..20".into(),
            offsets: vec![-12, 0, 12],
            launch_args: Vec::new(),
            gate_revision: "g".repeat(40),
        }
    }

    fn searched(path: &str) -> LayoutRecord {
        LayoutRecord {
            path: path.into(),
            sha256: "s".repeat(64),
            searched: Some(SearchStamp {
                rounds: 600,
                seed: 1,
            }),
        }
    }

    /// A baseline whose rows `rows` were blessed on `layouts` with tape `t1`.
    fn blessed(rows: &[&str], layouts: Option<Vec<LayoutRecord>>) -> Baseline {
        let mut base = Baseline {
            schema: 1,
            blessed_commit: "abc".into(),
            sdk_pin: "def".into(),
            ..Baseline::default()
        };
        for row in rows {
            base.speed.insert(
                row.to_string(),
                WindowBase {
                    offsets: vec![-12, 0, 12],
                    flips: vec![790, 786, 778],
                    work_per_frame: vec![100, 101, 102],
                    frame_work_p99: vec![10, 11, 12],
                    vblanks: vec![String::new(); 3],
                    layouts: layouts.clone(),
                    identity: Some(identity("t1")),
                    legacy_trusted: None,
                },
            );
        }
        base
    }

    /// A baseline blessed before any record existed.
    fn legacy(rows: &[&str]) -> Baseline {
        let mut base = blessed(rows, None);
        for w in base.speed.values_mut() {
            w.identity = None;
        }
        base
    }

    #[test]
    fn trust_legacy_adopts_a_pre_record_baseline_once_and_then_checks_identity() {
        let old = legacy(&["w"]);
        let e = kept_speed(&old, &now(&["w"], "t1"), false).unwrap_err();
        assert!(e.contains("--trust-legacy-rows"), "{e}");
        let kept = kept_speed(&old, &now(&["w"], "t1"), true).unwrap();
        assert_eq!(kept["w"].legacy_trusted.as_deref(), Some("abc"));
        assert_eq!(kept["w"].identity, Some(identity("t1")));
        assert_eq!(kept["w"].work_per_frame, old.speed["w"].work_per_frame);
        // The next landing keeps them without the flag, and a moved window still refuses.
        let mut next = old.clone();
        next.speed = kept;
        assert!(kept_speed(&next, &now(&["w"], "t1"), false).is_ok());
        assert!(kept_speed(&next, &now(&["w"], "t2"), false).is_err());
        // Adopting twice, or adopting a baseline that has records, is refused.
        assert!(kept_speed(&next, &now(&["w"], "t1"), true).is_err());
        assert!(kept_speed(&blessed(&["w"], Some(vec![])), &now(&["w"], "t1"), true).is_err());
        // Missing or extra rows still refuse.
        assert!(kept_speed(&old, &now(&["w", "x"], "t1"), true).is_err());
    }

    fn now(rows: &[&str], tape: &str) -> BTreeMap<String, WindowIdentity> {
        rows.iter()
            .map(|r| (r.to_string(), identity(tape)))
            .collect()
    }

    #[test]
    fn a_baseline_without_the_new_records_loads_and_keep_speed_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BASELINE_FILE),
            "schema = 1\nblessed_commit = \"abc\"\nsdk_pin = \"def\"\n\n[speed.w]\n\
             offsets = [0]\nflips = [790]\nwork_per_frame = [100]\nframe_work_p99 = [10]\n\
             vblanks = [\"\"]\n",
        )
        .unwrap();
        let old = Baseline::load(dir.path()).unwrap().unwrap();
        assert_eq!(old.speed["w"].layouts, None);
        assert_eq!(old.speed["w"].identity, None);
        let e = kept_speed(&old, &now(&["w"], "t1"), false).unwrap_err();
        assert!(
            e.contains("no layout provenance") && e.contains("[w]"),
            "{e}"
        );
        assert!(e.contains("plain `psoxide-test bless`"), "{e}");
    }

    #[test]
    fn keep_speed_refuses_rows_blessed_on_a_greedy_layout() {
        let mut greedy = searched("pgo/t.layout");
        greedy.searched = None;
        let old = blessed(&["w"], Some(vec![searched("pgo/g.layout"), greedy]));
        let e = kept_speed(&old, &now(&["w"], "t1"), false).unwrap_err();
        assert!(e.contains("`w`: pgo/t.layout"), "{e}");
        assert!(!e.contains("pgo/g.layout"), "{e}");
    }

    #[test]
    fn keep_speed_carries_stamped_rows_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let old = blessed(&["w", "w@mister"], Some(vec![searched("pgo/g.layout")]));
        old.save(dir.path()).unwrap();
        let old = Baseline::load(dir.path()).unwrap().unwrap();
        let mut new = Baseline {
            blessed_commit: "next".into(),
            ..old.clone()
        };
        new.speed = kept_speed(&old, &now(&["w", "w@mister"], "t1"), false).unwrap();
        new.save(dir.path()).unwrap();
        let back = Baseline::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.speed, old.speed);
        assert_eq!(
            back.speed["w"].layouts.as_ref().unwrap()[0].searched,
            Some(SearchStamp {
                rounds: 600,
                seed: 1
            })
        );
    }

    #[test]
    fn keep_speed_accepts_a_game_with_no_layout() {
        let old = blessed(&["w"], Some(Vec::new()));
        assert_eq!(
            kept_speed(&old, &now(&["w"], "t1"), false).unwrap(),
            old.speed
        );
    }

    #[test]
    fn keep_speed_refuses_a_changed_window_identity() {
        let old = blessed(&["w"], Some(vec![searched("pgo/g.layout")]));
        let e = kept_speed(&old, &now(&["w"], "t2"), false).unwrap_err();
        assert!(e.contains("`w` (tape sha256"), "{e}");
        let mut moved = now(&["w"], "t1");
        moved.get_mut("w").unwrap().offsets = vec![-6, 0, 6];
        let e = kept_speed(&old, &moved, false).unwrap_err();
        assert!(e.contains("offsets"), "{e}");
        let mut moved = now(&["w"], "t1");
        moved.get_mut("w").unwrap().gate_revision = "h".repeat(40);
        assert!(kept_speed(&old, &moved, false)
            .unwrap_err()
            .contains("gate revision"));
    }

    #[test]
    fn keep_speed_refuses_missing_and_extra_rows() {
        let old = blessed(&["w"], Some(vec![searched("pgo/g.layout")]));
        let e = kept_speed(&old, &now(&["w", "w@mister"], "t1"), false).unwrap_err();
        assert!(e.contains("lacks [w@mister]"), "{e}");
        let e = kept_speed(&old, &now(&[], "t1"), false).unwrap_err();
        assert!(e.contains("no longer declares [w]"), "{e}");
        // No rows declared and none blessed: nothing to keep, nothing refused.
        assert!(kept_speed(&blessed(&[], None), &now(&[], "t1"), false)
            .unwrap()
            .is_empty());
    }
}
