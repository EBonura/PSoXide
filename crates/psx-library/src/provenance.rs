//! The record an install leaves beside the disc, and the digests and timestamps it needs.

use crate::Error;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a disc's contents came from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Source {
    /// The GitHub repository, as `owner/name`.
    pub repository: String,
    /// The branch checked out, when one was (`None` on a detached head).
    pub branch: Option<String>,
    /// The full commit hash.
    pub commit: String,
    /// Whether tracked files differed from `commit` when the record was made.
    pub dirty: bool,
}

impl Source {
    /// Read the source from the git checkout at `directory`.
    ///
    /// # Errors
    ///
    /// [`Error::Git`] when `git` cannot run there or `directory` is not a checkout.
    pub fn from_git(directory: &Path, repository: &str) -> Result<Self, Error> {
        let commit = git(directory, &["rev-parse", "HEAD"])?;
        let branch = git(directory, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let changes = git(
            directory,
            &["status", "--porcelain", "--untracked-files=no"],
        )?;
        Ok(Self {
            repository: repository.to_string(),
            branch: (branch != "HEAD").then_some(branch),
            commit,
            dirty: !changes.is_empty(),
        })
    }

    fn to_json(&self) -> Value {
        let mut source = Map::new();
        source.insert("repository".into(), self.repository.clone().into());
        if let Some(branch) = &self.branch {
            source.insert("branch".into(), branch.clone().into());
        }
        source.insert("commit".into(), self.commit.clone().into());
        source.insert("dirty".into(), self.dirty.into());
        Value::Object(source)
    }
}

fn git(directory: &Path, arguments: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .map_err(|error| Error::Git(format!("cannot run git: {error}")))?;
    if !output.status.success() {
        return Err(Error::Git(format!(
            "git {} in {} failed: {}",
            arguments.join(" "),
            directory.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The lowercase hex SHA-256 of the file at `path`, read in blocks (discs run to hundreds of
/// megabytes).
///
/// # Errors
///
/// Any error opening or reading the file.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let count = file.read(&mut block)?;
        if count == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&block[..count]);
    }
}

/// `time` as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub(crate) fn utc_timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, remainder) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        remainder % 3600 / 60,
        remainder % 60
    )
}

/// The proleptic Gregorian date of the day `days` after 1970-01-01.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// What an install knows about the files it wrote.
pub(crate) struct Installed<'a> {
    pub bin_name: &'a str,
    pub bin_bytes: u64,
    pub bin_sha256: &'a str,
    pub cue_name: &'a str,
    pub cue_text: &'a str,
    pub source: &'a Source,
    pub installed_at: &'a str,
    /// The digest of the bin this install overwrote, when there was one.
    pub replaced_bin_sha256: Option<&'a str>,
}

/// The library record: `base` (the caller's own fields, if any) with the install's facts laid
/// over it, so the digests, names, source and timestamp can never be stale or contradicted.
///
/// ```json
/// { "artifacts": { "bin": { "file", "bytes", "sha256" }, "cue": { ... } },
///   "installed_at": "2026-10-23T12:00:00Z",
///   "source": { "repository", "branch", "commit", "dirty" },
///   "replaced": { "bin_sha256" } }
/// ```
///
/// `replaced` is only present when the previous bin differs from this one.
pub(crate) fn record(base: Option<&Value>, installed: &Installed<'_>) -> Result<Vec<u8>, Error> {
    let mut record = match base {
        Some(Value::Object(fields)) => Value::Object(fields.clone()),
        Some(_) => return Err(Error::ProvenanceBase),
        None => json!({}),
    };
    let mut facts = json!({
        "artifacts": {
            "bin": {
                "file": installed.bin_name,
                "bytes": installed.bin_bytes,
                "sha256": installed.bin_sha256,
            },
            "cue": {
                "file": installed.cue_name,
                "bytes": installed.cue_text.len(),
                "sha256": sha256_bytes(installed.cue_text.as_bytes()),
            },
        },
        "installed_at": installed.installed_at,
        "source": installed.source.to_json(),
    });
    if let Some(previous) = installed
        .replaced_bin_sha256
        .filter(|previous| *previous != installed.bin_sha256)
    {
        facts["replaced"] = json!({ "bin_sha256": previous });
    } else if let Some(fields) = record.as_object_mut() {
        // A stale "replaced" in the base would claim a replacement that did not happen.
        fields.remove("replaced");
    }
    overlay(&mut record, facts);
    Ok((serde_json::to_string_pretty(&record)? + "\n").into_bytes())
}

/// Merge `over` into `base`: objects merge key by key, anything else in `over` wins.
fn overlay(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => overlay(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn source() -> Source {
        Source {
            repository: "EBonura/repo".into(),
            branch: Some("main".into()),
            commit: "0123456789abcdef".into(),
            dirty: false,
        }
    }

    fn installed<'a>(source: &'a Source, replaced: Option<&'a str>) -> Installed<'a> {
        Installed {
            bin_name: "Game Name (repo).bin",
            bin_bytes: 4,
            bin_sha256: "binsha",
            cue_name: "Game Name (repo).cue",
            cue_text: "FILE \"Game Name (repo).bin\" BINARY\n",
            source,
            installed_at: "2026-10-23T00:00:00Z",
            replaced_bin_sha256: replaced,
        }
    }

    fn parse(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn the_record_states_digests_source_time_and_the_replaced_bin() {
        let source = source();
        let value = parse(&record(None, &installed(&source, Some("oldsha"))).unwrap());
        assert_eq!(value["artifacts"]["bin"]["file"], "Game Name (repo).bin");
        assert_eq!(value["artifacts"]["bin"]["bytes"], 4);
        assert_eq!(value["artifacts"]["bin"]["sha256"], "binsha");
        assert_eq!(value["artifacts"]["cue"]["file"], "Game Name (repo).cue");
        assert_eq!(
            value["artifacts"]["cue"]["sha256"],
            sha256_bytes(b"FILE \"Game Name (repo).bin\" BINARY\n")
        );
        assert_eq!(value["installed_at"], "2026-10-23T00:00:00Z");
        assert_eq!(value["source"]["commit"], "0123456789abcdef");
        assert_eq!(value["source"]["branch"], "main");
        assert_eq!(value["source"]["dirty"], false);
        assert_eq!(value["replaced"]["bin_sha256"], "oldsha");
    }

    #[test]
    fn replacing_an_identical_bin_or_nothing_records_no_replacement() {
        let source = source();
        for replaced in [None, Some("binsha")] {
            let base = json!({ "replaced": { "source": "claimed" } });
            let value = parse(&record(Some(&base), &installed(&source, replaced)).unwrap());
            assert!(value.get("replaced").is_none(), "{value}");
        }
    }

    #[test]
    fn the_base_keeps_its_own_fields_but_never_overrides_the_install_facts() {
        let source = source();
        let base = json!({
            "artifacts": {
                "bin": { "file": "stale.bin", "sha256": "stale" },
                "exe": { "file": "game.exe", "sha256": "exe" },
            },
            "source": { "note": "shipping build", "commit": "stale" },
            "installed_at": "long ago",
            "gates": "journey 11/11",
        });
        let value = parse(&record(Some(&base), &installed(&source, Some("oldsha"))).unwrap());
        assert_eq!(value["artifacts"]["bin"]["file"], "Game Name (repo).bin");
        assert_eq!(value["artifacts"]["bin"]["sha256"], "binsha");
        assert_eq!(value["artifacts"]["exe"]["file"], "game.exe");
        assert_eq!(value["source"]["note"], "shipping build");
        assert_eq!(value["source"]["commit"], "0123456789abcdef");
        assert_eq!(value["installed_at"], "2026-10-23T00:00:00Z");
        assert_eq!(value["gates"], "journey 11/11");
        assert_eq!(value["replaced"]["bin_sha256"], "oldsha");
    }

    #[test]
    fn a_detached_head_records_no_branch() {
        let mut detached = source();
        detached.branch = None;
        detached.dirty = true;
        let value = parse(&record(None, &installed(&detached, None)).unwrap());
        assert!(value["source"].get("branch").is_none());
        assert_eq!(value["source"]["dirty"], true);
    }

    #[test]
    fn a_base_that_is_not_an_object_is_rejected() {
        let source = source();
        let error = record(Some(&json!([1])), &installed(&source, None)).unwrap_err();
        assert!(matches!(error, Error::ProvenanceBase), "{error}");
    }

    #[test]
    fn timestamps_are_utc_iso_8601() {
        let at = |seconds| utc_timestamp(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_781_000_000 + 86_399), "2026-06-10T10:13:19Z");
        assert_eq!(at(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn the_digest_of_a_file_matches_the_digest_of_its_bytes() {
        let path = std::env::temp_dir().join(format!("psx-library-sha-{}", std::process::id()));
        let bytes = vec![7u8; (1 << 20) + 5];
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(&bytes));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_checkout_yields_its_commit_and_a_non_repository_is_an_error() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Ok(source) = Source::from_git(manifest, "EBonura/PSoXide") {
            assert_eq!(source.commit.len(), 40);
        }
        let outside =
            std::env::temp_dir().join(format!("psx-library-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let error = Source::from_git(&outside, "x/y").unwrap_err();
        assert!(matches!(error, Error::Git(_)), "{error}");
        std::fs::remove_dir_all(&outside).unwrap();
    }
}
