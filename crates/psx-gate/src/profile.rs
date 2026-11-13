//! `sdk/profile.toml` (what a guest build may enable, the probe programs, the deprecated
//! symbols, and the exceptions of the programs inside the SDK repository) and a program's own
//! `psx-gate.toml` (its `[[exception]]` entries only).
//!
//! This file is also compiled into the build script of every SDK crate that has features
//! (`#[path]`), so it uses `std` only and reads the small TOML subset the files are written in:
//! `[table]`, `[[exception]]`, `key = "string"`, `key = 12` and one-line `key = ["a", "b"]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Features by crate name.
pub type FeatureTable = BTreeMap<String, Vec<String>>;

/// One thing a program does today that the standard forbids, listed so nothing new joins it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Exception {
    /// The program: the name of its root package (`no-receipt`: the name the disc builder is
    /// given for the executable). Empty in a program's own file when the entry applies to every
    /// program of that repository.
    pub program: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// `crate/feature`, a file relative to the program's workspace root, a package name, or `-`.
    pub item: String,
    /// Hit lines in the file (the [`COUNTED`] kinds).
    pub count: Option<u32>,
    /// A short tag for what removes it: a workstream (`W3`), `unscheduled` or `decision`.
    pub remove: String,
}

/// Every kind of exception.
pub const KINDS: [&str; 7] = [
    "feature",
    "register-literal",
    "vector-write",
    "cop0-asm",
    "deprecated-use",
    "private-copy",
    "no-receipt",
];

/// The kinds that count hit lines in a file.
pub const COUNTED: [&str; 4] = [
    "register-literal",
    "vector-write",
    "cop0-asm",
    "deprecated-use",
];

/// The parsed profile.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Profile {
    /// Allowed in any guest build.
    pub shipping: FeatureTable,
    /// Allowed, and marks the build as a diagnostic one (it cannot become a disc).
    pub diag: FeatureTable,
    /// Allowed only in host (non-MIPS) builds, which the build scripts do not check.
    pub host: FeatureTable,
    /// SDK-local programs that measure the hardware: exempt from the register-literal,
    /// vector-write and COP0 rules (features, private copies and deprecated symbols are still
    /// checked).
    pub probe: Vec<String>,
    /// Legacy SDK entry points a program may still call only while it lists them.
    pub deprecated: Vec<String>,
    /// Exceptions: of the programs inside the SDK repository (`sdk/profile.toml`) or of one
    /// program (its `psx-gate.toml`).
    pub exceptions: Vec<Exception>,
    /// The file this was read from.
    pub path: PathBuf,
}

fn unquote(text: &str) -> Result<String, String> {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_string)
        .ok_or_else(|| format!("expected a quoted string, found `{text}`"))
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_array(text: &str) -> Result<Vec<String>, String> {
    let inner = text
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| format!("expected a one-line array, found `{text}`"))?;
    inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(unquote)
        .collect()
}

impl Exception {
    /// An exception entry.
    #[must_use]
    pub fn new(program: &str, kind: &str, item: &str, count: Option<u32>, remove: &str) -> Self {
        Self {
            program: program.to_string(),
            kind: kind.to_string(),
            item: item.to_string(),
            count,
            remove: remove.to_string(),
        }
    }
}

impl Profile {
    /// Parse the text of a profile; `path` is only recorded.
    ///
    /// # Errors
    /// A line outside the supported subset, with its number.
    pub fn parse(text: &str, path: &Path) -> Result<Self, String> {
        Self::parse_as(text, path, false)
    }

    /// Parse the text of a program's own `psx-gate.toml`: `[[exception]]` entries only, and an
    /// entry may leave out `program`.
    ///
    /// # Errors
    /// A line outside the supported subset, a table other than `[[exception]]`.
    pub fn parse_program_file(text: &str, path: &Path) -> Result<Self, String> {
        Self::parse_as(text, path, true)
    }

    fn parse_as(text: &str, path: &Path, program_file: bool) -> Result<Self, String> {
        let mut profile = Profile {
            path: path.to_path_buf(),
            ..Profile::default()
        };
        let mut section = String::new();
        let mut current: Option<BTreeMap<String, String>> = None;
        let flush = |profile: &mut Profile, fields: Option<BTreeMap<String, String>>| {
            let Some(mut f) = fields else { return Ok(()) };
            let mut take = |key: &str| {
                f.remove(key)
                    .ok_or(format!("an [[exception]] has no `{key}`"))
            };
            let exception = Exception {
                program: if program_file {
                    take("program").unwrap_or_default()
                } else {
                    take("program")?
                },
                kind: take("kind")?,
                item: take("item")?,
                count: match f.remove("count") {
                    Some(n) => Some(
                        n.parse()
                            .map_err(|_| format!("count `{n}` is not a number"))?,
                    ),
                    None => None,
                },
                remove: f
                    .remove("remove")
                    .ok_or("an [[exception]] has no `remove`")?,
            };
            profile.exceptions.push(exception);
            Ok::<(), String>(())
        };
        for (number, raw) in text.lines().enumerate() {
            let at = |e: String| format!("{}:{}: {e}", path.display(), number + 1);
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
                flush(&mut profile, current.take()).map_err(at)?;
                if name.trim() != "exception" {
                    return Err(at(format!("unknown table [[{name}]]")));
                }
                section = "exception".into();
                current = Some(BTreeMap::new());
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                flush(&mut profile, current.take()).map_err(at)?;
                section = name.trim().to_string();
                let known = ["shipping", "diag", "host", "probe", "deprecated"];
                if program_file || !known.contains(&section.as_str()) {
                    return Err(at(format!("unknown table [{section}]")));
                }
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at("expected `key = value`".into()))?;
            let key = key.trim().to_string();
            if section == "exception" {
                let value = if value.trim().starts_with('"') {
                    unquote(value).map_err(at)?
                } else {
                    value.trim().to_string()
                };
                if let Some(fields) = current.as_mut() {
                    fields.insert(key, value);
                }
            } else {
                let table = match section.as_str() {
                    "probe" | "deprecated" => {
                        let (wanted, list) = if section == "probe" {
                            ("programs", &mut profile.probe)
                        } else {
                            ("symbols", &mut profile.deprecated)
                        };
                        if key != wanted {
                            return Err(at(format!("[{section}] holds `{wanted}`, not `{key}`")));
                        }
                        *list = parse_array(value).map_err(at)?;
                        continue;
                    }
                    "shipping" => &mut profile.shipping,
                    "diag" => &mut profile.diag,
                    "host" => &mut profile.host,
                    _ => return Err(at("a key outside any table".into())),
                };
                table.insert(key, parse_array(value).map_err(at)?);
            }
        }
        flush(&mut profile, current.take()).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(profile)
    }

    /// Read and parse `path`.
    ///
    /// # Errors
    /// The file cannot be read, or [`Profile::parse`] refuses it.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text, path)
    }

    /// The profile of the SDK that `start` (a directory inside it) belongs to: the nearest
    /// ancestor holding `sdk/profile.toml`.
    ///
    /// # Errors
    /// No ancestor holds one.
    pub fn find_from(start: &Path) -> Result<PathBuf, String> {
        start
            .ancestors()
            .map(|dir| dir.join("sdk").join("profile.toml"))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| format!("no sdk/profile.toml above {}", start.display()))
    }

    fn lists(table: &FeatureTable, krate: &str, feature: &str) -> bool {
        table
            .get(krate)
            .is_some_and(|list| list.iter().any(|f| f == feature))
    }

    /// Is `feature` of `krate` in `[shipping]`?
    #[must_use]
    pub fn is_shipping(&self, krate: &str, feature: &str) -> bool {
        Self::lists(&self.shipping, krate, feature)
    }

    /// Is `feature` of `krate` in `[diag]`?
    #[must_use]
    pub fn is_diag(&self, krate: &str, feature: &str) -> bool {
        Self::lists(&self.diag, krate, feature)
    }

    /// Is `feature` of `krate` in `[host]`?
    #[must_use]
    pub fn is_host(&self, krate: &str, feature: &str) -> bool {
        Self::lists(&self.host, krate, feature)
    }

    /// Does `krate/feature` have a `feature` entry for `program` (`None`: for any program of this
    /// list)? The SDK's profile lists only programs inside the SDK repository; a program's file
    /// lists its own.
    #[must_use]
    pub fn feature_excepted(&self, krate: &str, feature: &str, program: Option<&str>) -> bool {
        let item = format!("{krate}/{feature}");
        self.exceptions.iter().any(|e| {
            e.kind == "feature"
                && e.item == item
                && program.is_none_or(|p| e.program.is_empty() || e.program == p)
        })
    }

    /// Does a table of this profile list the feature (shipping, diag or host)?
    #[must_use]
    pub fn lists_feature(&self, krate: &str, feature: &str) -> bool {
        self.is_shipping(krate, feature)
            || self.is_diag(krate, feature)
            || self.is_host(krate, feature)
    }

    /// The `no-receipt` entry for the executable called `label`, if any.
    #[must_use]
    pub fn no_receipt(&self, label: &str) -> Option<&Exception> {
        self.exceptions
            .iter()
            .find(|e| e.kind == "no-receipt" && (e.program.is_empty() || e.program == label))
    }
}

#[must_use]
/// The feature names a crate's manifest declares (`default` excluded), by a line scan of its
/// `[features]` table.
pub fn declared_features(manifest: &str) -> Vec<String> {
    let mut in_features = false;
    let mut names = Vec::new();
    for raw in manifest.lines() {
        let line = strip_comment(raw).trim();
        if let Some(table) = line.strip_prefix('[') {
            in_features = table.trim_end_matches(']').trim() == "features";
            continue;
        }
        if in_features && !raw.starts_with(char::is_whitespace) {
            if let Some((name, _)) = line.split_once('=') {
                let name = name.trim();
                if !name.is_empty() && name != "default" {
                    names.push(name.trim_matches('"').to_string());
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[shipping]
psx-rt = ["runtime", "alloc"]   # comment
[diag]
psx-rt = ["host-tty"]
[host]
psx-grid = ["std"]
[probe]
programs = ["probe-a", "probe-b"]
[deprecated]
symbols = ["old_call"]

[[exception]]
program = "game"
kind = "feature"
item = "psx-rt/alloc"
remove = "W9"

[[exception]]
program = "game"
kind = "register-literal"
item = "game/src/a.rs"
count = 3
remove = "W2"
"#;

    #[test]
    fn parses_tables_and_exceptions() {
        let p = Profile::parse(SAMPLE, Path::new("p.toml")).unwrap();
        assert!(p.is_shipping("psx-rt", "alloc") && p.is_diag("psx-rt", "host-tty"));
        assert!(p.is_host("psx-grid", "std") && !p.is_shipping("psx-rt", "host-tty"));
        assert_eq!(p.exceptions.len(), 2);
        assert_eq!(p.exceptions[1].count, Some(3));
        assert!(p.feature_excepted("psx-rt", "alloc", Some("game")));
        assert!(!p.feature_excepted("psx-rt", "alloc", Some("other")));
        assert!(!p.feature_excepted("psx-rt", "strict-faults", None));
        assert!(
            p.lists_feature("psx-rt", "host-tty") && !p.lists_feature("psx-rt", "strict-faults")
        );
        assert_eq!(p.probe, ["probe-a", "probe-b"]);
        assert_eq!(p.deprecated, ["old_call"]);
    }

    #[test]
    fn a_program_file_holds_exceptions_only_and_may_leave_out_the_program() {
        let text = "[[exception]]\nkind = \"no-receipt\"\nitem = \"-\"\nremove = \"W9\"\n";
        let own = Profile::parse_program_file(text, Path::new("psx-gate.toml")).unwrap();
        assert!(own.no_receipt("anything").is_some());
        assert!(Profile::parse(text, Path::new("p")).is_err());
        assert!(Profile::parse_program_file("[shipping]\n", Path::new("p")).is_err());
    }

    #[test]
    fn refuses_what_it_does_not_read() {
        assert!(Profile::parse("[nope]\n", Path::new("p")).is_err());
        assert!(Profile::parse("[[exception]]\nprogram = \"a\"\n", Path::new("p")).is_err());
        assert!(Profile::parse("psx-rt = []\n", Path::new("p")).is_err());
        assert!(Profile::parse("[probe]\nsymbols = []\n", Path::new("p")).is_err());
    }

    #[test]
    fn reads_declared_features() {
        let manifest = "[package]\nname = \"x\"\n[features]\ndefault = [\"a\"]\na = []\nb = [\n  \"a\",\n]\n# c = []\n[dependencies]\nz = 1\n";
        assert_eq!(declared_features(manifest), ["a", "b"]);
    }
}
