//! `psx-gate`: the architecture gate over one program (G2), run by `psoxide-pgo apply` before it
//! compiles and runnable on its own.
//!
//! Input is `cargo metadata` of the program (resolved with the program's own feature flags), the
//! SDK it is built against (`sdk/profile.toml`: the feature tables, the probe programs, the
//! deprecated symbols, and the exceptions of the programs inside the SDK repository) and the
//! program's own `psx-gate.toml` at its repository root. It refuses:
//!
//! * a feature of an SDK crate that `[shipping]` and `[diag]` do not list (sdk/docs/GATE.md rule 2);
//! * a hardware register address, an exception-vector write or COP0 assembly in the program's
//!   own sources, and a private copy of an SDK crate in its graph (rule 3);
//! * a call of a deprecated SDK entry point.
//!
//! Today's violations are listed as exceptions (a ratchet): a violation not listed is refused, a
//! listed one that is gone or has fewer hits than its count is refused too, so the list can only
//! shrink. A program inside the SDK repository is listed in the SDK's profile; any other
//! program is listed in its own file, and the SDK names none of them.

use psx_gate::profile::{Exception, Profile, COUNTED};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// What the gate found about one program.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// The program's identity in its receipt: its root package, behind the repository name when
    /// it is not inside the SDK repository.
    pub program: String,
    /// The program's root package name (the `program` of its entries).
    pub package: String,
    /// The program lives inside the SDK repository (its entries are in `sdk/profile.toml`).
    pub sdk_local: bool,
    /// The program's own `psx-gate.toml`, when it has one.
    pub program_file: Option<PathBuf>,
    /// A `[diag]` feature of an SDK crate is on, so the image is not for a disc.
    pub diag: bool,
    /// The `crate/feature` entries that made it a diag build.
    pub diag_features: Vec<String>,
    /// Each refusal, ready to print.
    pub violations: Vec<String>,
    /// Every violation as the exception that would list it (`--propose`).
    pub proposed: Vec<Exception>,
    /// How many exceptions each kind used, for the report table.
    pub held: BTreeMap<String, u32>,
}

/// One raw hit before the ratchet.
#[derive(Clone, Debug)]
struct Hit {
    kind: &'static str,
    item: String,
    line: Option<u32>,
    text: String,
}

const REGISTER: &str = r"(?i)0x1f80|0xbf80|0x9f80|1f80_|1f80[0-9a-f]{4}h";
const VECTOR: &str = r"(?i)0x[0-9a-f_]*(8000|a000)_?0080|declare_stack_safe_handler";
const COP0: &str = r"\b(mfc0|mtc0|rfe|cfc0|ctc0)\b";

/// The sdk/docs/GATE.md rule and the remedy for a kind.
fn advice(kind: &str) -> &'static str {
    match kind {
        "feature" => {
            "sdk/docs/GATE.md rule 2 (no cargo feature selects a different implementation); \
             use the SDK's one implementation, or a [diag] tap that no shipping build enables"
        }
        "register-literal" => {
            "sdk/docs/GATE.md rule 3 (a game does not touch a register); use the psx_hw / psx_io \
             accessor for it, or add the job to the SDK"
        }
        "vector-write" => {
            "sdk/docs/GATE.md rule 3 (a game does not wrap the exception vector); register with \
             psx_rt::interrupts (the dispatcher) instead"
        }
        "cop0-asm" => {
            "sdk/docs/GATE.md rule 3 (a game does not run COP0 code); use psx_rt::interrupts and \
             psx_rt's critical sections"
        }
        "deprecated-use" => {
            "a legacy SDK entry point (sdk/profile.toml [deprecated]); use the SDK's current \
             API for the job"
        }
        "private-copy" => {
            "sdk/docs/GATE.md rules 1 and 3 (one implementation, no game-side copy); depend on the \
             SDK's crate of that name and delete the copy"
        }
        _ => "",
    }
}

/// Code of `text` with comments blanked, one entry per line (block comments span lines).
fn code_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = 0usize;
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut kept = String::new();
        let mut quoted = false;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if in_block > 0 {
                if c == '*' && next == Some('/') {
                    in_block -= 1;
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    in_block += 1;
                    i += 1;
                }
            } else if quoted {
                kept.push(c);
                if c == '\\' {
                    if let Some(n) = next {
                        kept.push(n);
                        i += 1;
                    }
                } else if c == '"' {
                    quoted = false;
                }
            } else if c == '/' && next == Some('/') {
                break;
            } else if c == '/' && next == Some('*') {
                in_block = 1;
                i += 1;
            } else {
                if c == '"' {
                    quoted = true;
                }
                kept.push(c);
            }
            i += 1;
        }
        out.push(kept);
    }
    out
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name.starts_with('.')
                || matches!(name.as_str(), "target" | "tests" | "benches" | "examples")
            {
                continue;
            }
            rust_files(&path, files);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "s" | "S")
        ) {
            files.push(path);
        }
    }
}

/// The files of a package's own guest code: the tree under each lib or bin target's source
/// directory, and each build script.
fn package_sources(package: &Value) -> Vec<PathBuf> {
    let manifest_dir = package["manifest_path"]
        .as_str()
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let mut files = Vec::new();
    for target in package["targets"].as_array().into_iter().flatten() {
        let kinds: Vec<&str> = target["kind"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let Some(src) = target["src_path"].as_str().map(PathBuf::from) else {
            continue;
        };
        if kinds.contains(&"custom-build") {
            files.push(src);
        } else if kinds
            .iter()
            .any(|k| ["lib", "rlib", "bin", "staticlib", "cdylib"].contains(k))
        {
            match src.parent() {
                Some(dir) if dir != manifest_dir => rust_files(dir, &mut files),
                _ => files.push(src),
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

fn is_sdk_name(name: &str) -> bool {
    name.starts_with("psx-") || name.starts_with("psxed") || name.starts_with("psoxide")
}

fn relative(path: &Path, root: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(root)
        .unwrap_or(&path)
        .display()
        .to_string()
}

/// Run the gate on `metadata` (`cargo metadata --format-version 1`, resolved) for the program
/// whose root package is `root_name` (`None`: the metadata's own root, else the only workspace
/// member). `repo` is the program's repository name (the identity is `repo/package`, because
/// two repositories may call their root package alike). `sdk_root` is the SDK the build links,
/// which holds `sdk/profile.toml`; `own` is the program's `psx-gate.toml`.
///
/// # Errors
/// The metadata does not name one root package, or has no resolve.
///
/// # Panics
/// Never in practice: the only `expect`s are on constant patterns.
pub fn check(
    metadata: &Value,
    root_name: Option<&str>,
    repo: Option<&str>,
    sdk_root: &Path,
    profile: &Profile,
    own: Option<&Profile>,
) -> Result<Outcome, String> {
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or("metadata has no packages")?
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata has no resolve")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let name_of = |id: &str| packages[id]["name"].as_str().unwrap_or("").to_string();
    let root_id = match root_name {
        Some(name) => packages
            .keys()
            .copied()
            .find(|id| packages[id]["source"].is_null() && name_of(id) == name)
            .ok_or_else(|| format!("no workspace package named {name}"))?
            .to_string(),
        None => match metadata["resolve"]["root"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let members = metadata["workspace_default_members"]
                    .as_array()
                    .or_else(|| metadata["workspace_members"].as_array())
                    .ok_or("no workspace members")?;
                match members.as_slice() {
                    [one] => one.as_str().ok_or("bad member")?.to_string(),
                    _ => return Err("the workspace has several packages; name one with -p".into()),
                }
            }
        },
    };
    let package_name = name_of(&root_id);
    let workspace_dir = Path::new(metadata["workspace_root"].as_str().unwrap_or(""));
    // Items are named from the repository root, so a crate shared by several workspaces of
    // one repository (a `shared/` directory) has one name in all of them.
    let repo_dir = git_toplevel(workspace_dir).unwrap_or_else(|| workspace_dir.to_path_buf());
    let workspace_root = repo_dir.as_path();
    let sdk_root = sdk_root
        .canonicalize()
        .unwrap_or_else(|_| sdk_root.to_path_buf());
    // A program inside the SDK repository is named by its package alone and listed in the SDK's
    // profile; every other program is `repo/package` and listed in its own file.
    let sdk_local = repo_dir == sdk_root || workspace_dir.starts_with(&sdk_root);
    let program = match repo {
        Some(repo) if !sdk_local => format!("{repo}/{package_name}"),
        _ => package_name.clone(),
    };
    let probe = sdk_local && profile.probe.contains(&package_name);
    let listing = if sdk_local {
        "sdk/profile.toml"
    } else {
        "psx-gate.toml"
    };

    // Reachable normal and build dependencies of the root.
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root_id.clone()];
    while let Some(id) = stack.pop() {
        if !reached.insert(id.clone()) {
            continue;
        }
        for dep in nodes
            .get(id.as_str())
            .and_then(|n| n["deps"].as_array())
            .into_iter()
            .flatten()
        {
            let only_dev = dep["dep_kinds"]
                .as_array()
                .is_some_and(|k| !k.is_empty() && k.iter().all(|k| k["kind"] == "dev"));
            if !only_dev {
                if let Some(pkg) = dep["pkg"].as_str() {
                    stack.push(pkg.to_string());
                }
            }
        }
    }
    let manifest_of = |id: &str| -> PathBuf {
        let m = Path::new(packages[id]["manifest_path"].as_str().unwrap_or(""));
        let m = m.parent().unwrap_or(m);
        m.canonicalize().unwrap_or_else(|_| m.to_path_buf())
    };
    let under_sdk = |dir: &Path| dir.starts_with(&sdk_root);
    let is_example = |dir: &Path| {
        dir.strip_prefix(&sdk_root)
            .map(|rel| rel.components().any(|c| c.as_os_str() == "examples"))
            .unwrap_or(false)
    };
    let mut hits: Vec<Hit> = Vec::new();
    let mut outcome = Outcome {
        program: program.clone(),
        package: package_name.clone(),
        sdk_local,
        ..Outcome::default()
    };
    let (register, vector, cop0) = (
        Regex::new(REGISTER).expect("valid"),
        Regex::new(VECTOR).expect("valid"),
        Regex::new(COP0).expect("valid"),
    );
    let deprecated = (!profile.deprecated.is_empty()).then(|| {
        let names: Vec<String> = profile
            .deprecated
            .iter()
            .map(|s| regex::escape(s))
            .collect();
        Regex::new(&format!(r"\b({})\b", names.join("|"))).expect("escaped names")
    });
    // A probe program measures the hardware: it is exempt from the three line rules.
    let mut line_rules: Vec<(&'static str, &Regex)> = Vec::new();
    if !probe {
        line_rules.extend([
            ("register-literal", &register),
            ("vector-write", &vector),
            ("cop0-asm", &cop0),
        ]);
    }
    if let Some(pattern) = &deprecated {
        line_rules.push(("deprecated-use", pattern));
    }
    for id in &reached {
        let dir = manifest_of(id);
        let package = packages[id.as_str()];
        let name = name_of(id);
        let local = package["source"].is_null();
        let sdk_package = under_sdk(&dir) && !is_example(&dir);
        if sdk_package {
            let enabled = nodes[id.as_str()]["features"]
                .as_array()
                .into_iter()
                .flatten();
            let declared = &package["features"];
            for feature in enabled.filter_map(Value::as_str) {
                if feature == "default" || declared.get(feature).is_none() {
                    continue;
                }
                if profile.is_shipping(&name, feature) {
                    continue;
                }
                if profile.is_diag(&name, feature) {
                    outcome.diag = true;
                    outcome.diag_features.push(format!("{name}/{feature}"));
                    continue;
                }
                hits.push(Hit {
                    kind: "feature",
                    item: format!("{name}/{feature}"),
                    line: None,
                    text: if profile.is_host(&name, feature) {
                        "a host-only feature in a guest build".into()
                    } else {
                        "not in sdk/profile.toml".into()
                    },
                });
            }
            continue;
        }
        if is_sdk_name(&name) {
            hits.push(Hit {
                kind: "private-copy",
                item: name.clone(),
                line: None,
                text: format!(
                    "package `{name}` at {} is not the SDK's",
                    relative(&dir, workspace_root)
                ),
            });
        }
        if !local {
            continue;
        }
        for file in package_sources(package) {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let item = relative(&file, workspace_root);
            for (n, code) in code_lines(&text).iter().enumerate() {
                for (kind, pattern) in &line_rules {
                    if pattern.is_match(code) {
                        hits.push(Hit {
                            kind,
                            item: item.clone(),
                            line: Some(n as u32 + 1),
                            text: code.trim().chars().take(100).collect(),
                        });
                    }
                }
            }
        }
    }

    // The ratchet.
    let listed_here = if sdk_local { Some(profile) } else { own };
    let mine: Vec<&Exception> = listed_here
        .into_iter()
        .flat_map(|list| list.exceptions.iter())
        .filter(|e| e.program == package_name || (!sdk_local && e.program.is_empty()))
        .collect();
    let mut groups: BTreeMap<(&str, String), Vec<&Hit>> = BTreeMap::new();
    for hit in &hits {
        groups
            .entry((hit.kind, hit.item.clone()))
            .or_default()
            .push(hit);
    }
    for ((kind, item), found) in &groups {
        let counted = COUNTED.contains(kind);
        outcome.proposed.push(Exception::new(
            &package_name,
            kind,
            item,
            counted.then_some(found.len() as u32),
            "TODO",
        ));
        let listed = mine.iter().find(|e| e.kind == *kind && &e.item == item);
        let at = |hit: &Hit| match hit.line {
            Some(line) => format!("{item}:{line}"),
            None => item.clone(),
        };
        match listed {
            None => {
                for hit in found {
                    outcome.violations.push(format!(
                        "psx-gate: {program}: {}: {} ({kind}); {}",
                        at(hit),
                        hit.text,
                        advice(kind)
                    ));
                }
            }
            Some(e) if counted && found.len() as u32 > e.count.unwrap_or(0) => {
                let lines: Vec<String> = found
                    .iter()
                    .map(|h| format!("{} {}", at(h), h.text))
                    .collect();
                outcome.violations.push(format!(
                    "psx-gate: {program}: {item}: {} {kind} lines, {listing} lists {} (a \
                     count only goes down); {}. The lines now:\n    {}",
                    found.len(),
                    e.count.unwrap_or(0),
                    advice(kind),
                    lines.join("\n    ")
                ));
            }
            Some(e) if counted && (found.len() as u32) < e.count.unwrap_or(0) => {
                outcome.violations.push(format!(
                    "psx-gate: {program}: {item}: {} {kind} lines, {listing} lists {}: \
                     lower the count to {} (it removes with {})",
                    found.len(),
                    e.count.unwrap_or(0),
                    found.len(),
                    e.remove
                ));
            }
            Some(_) => {
                *outcome.held.entry((*kind).to_string()).or_default() += 1;
            }
        }
    }
    for e in &mine {
        if !groups.contains_key(&(e.kind.as_str(), e.item.clone())) && e.kind != "no-receipt" {
            outcome.violations.push(format!(
                "psx-gate: {program}: {listing} lists `{} {}` ({}) but the program no \
                 longer does it: delete the entry",
                e.kind, e.item, e.remove
            ));
        }
    }
    Ok(outcome)
}

/// `psx-gate [--sdk DIR] [--metadata FILE | --crate DIR] [--package NAME] [--program-file FILE]
/// [-- CARGO-ARGS]`:
/// run the gate and print the refusals. Exit 1 when there are any.
pub fn main(args: &[String]) -> i32 {
    let mut sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (mut metadata, mut crate_dir, mut package) = (None, PathBuf::from("."), None);
    let mut cargo: Vec<String> = Vec::new();
    let mut propose = false;
    let mut program_file: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--sdk" => sdk = PathBuf::from(it.next().cloned().unwrap_or_default()),
            "--metadata" => metadata = it.next().map(PathBuf::from),
            "--crate" => crate_dir = PathBuf::from(it.next().cloned().unwrap_or_default()),
            "--package" => package = it.next().cloned(),
            "--propose" => propose = true,
            "--program-file" => program_file = it.next().map(PathBuf::from),
            "--" => {
                cargo.extend(it.by_ref().cloned());
            }
            other => {
                eprintln!("psx-gate: unknown argument {other}");
                return 2;
            }
        }
    }
    match run(
        &sdk,
        metadata.as_deref(),
        &crate_dir,
        package.as_deref(),
        &cargo,
        program_file.as_deref(),
    ) {
        Ok(outcome) if propose => {
            for e in &outcome.proposed {
                println!(
                    "[[exception]]\nprogram = \"{}\"\nkind = \"{}\"\nitem = \"{}\"",
                    e.program, e.kind, e.item
                );
                if let Some(n) = e.count {
                    println!("count = {n}");
                }
                println!("remove = \"{}\"\n", e.remove);
            }
            0
        }
        Ok(outcome) => {
            for line in &outcome.violations {
                eprintln!("{line}");
            }
            if outcome.violations.is_empty() {
                println!(
                    "psx-gate: {} passes ({}; exceptions held: {})",
                    outcome.program,
                    if outcome.diag {
                        format!("diag build: {}", outcome.diag_features.join(" "))
                    } else {
                        "shipping build".to_string()
                    },
                    outcome.held.values().sum::<u32>()
                );
                0
            } else {
                eprintln!(
                    "psx-gate: {} refused: {} problem(s)",
                    outcome.program,
                    outcome.violations.len()
                );
                1
            }
        }
        Err(e) => {
            eprintln!("psx-gate: {e}");
            2
        }
    }
}

/// Resolve the program with cargo (or read `metadata`) and check it. The program's own file is
/// `program_file`, else `psx-gate.toml` at the root of its repository when there is one.
///
/// # Errors
/// Cargo or a profile cannot be read, or the program is ambiguous.
pub fn run(
    sdk: &Path,
    metadata: Option<&Path>,
    crate_dir: &Path,
    package: Option<&str>,
    cargo_args: &[String],
    program_file: Option<&Path>,
) -> Result<Outcome, String> {
    let profile = Profile::read(&sdk.join("sdk").join("profile.toml"))?;
    let value: Value = match metadata {
        Some(file) => serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
        None => cargo_metadata(crate_dir, cargo_args)?,
    };
    let mut named = package.map(str::to_string);
    if named.is_none() {
        named = cargo_package(cargo_args);
    }
    let workspace = value["workspace_root"].as_str().unwrap_or(".");
    let repo = repo_name(Path::new(workspace));
    let repo_dir = git_toplevel(Path::new(workspace)).unwrap_or_else(|| PathBuf::from(workspace));
    let file = match program_file {
        Some(file) => Some(file.to_path_buf()),
        None => Some(repo_dir.join("psx-gate.toml")).filter(|file| file.is_file()),
    };
    let own = match &file {
        Some(file) => {
            let text = fs::read_to_string(file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            Some(Profile::parse_program_file(&text, file)?)
        }
        None => None,
    };
    let mut outcome = check(
        &value,
        named.as_deref(),
        repo.as_deref(),
        sdk,
        &profile,
        own.as_ref(),
    )?;
    outcome.program_file = file.filter(|_| !outcome.sdk_local);
    Ok(outcome)
}

fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let top = Path::new(text.trim());
    (output.status.success() && !text.trim().is_empty())
        .then(|| top.canonicalize().unwrap_or_else(|_| top.to_path_buf()))
}

/// The repository name of the checkout at `dir`: the last segment of its `origin` URL.
fn repo_name(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    let url = String::from_utf8(output.stdout).ok()?;
    let name = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

/// `-p NAME` or `--package NAME` in the program's cargo arguments.
#[must_use]
pub fn cargo_package(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-p" || a == "--package" {
            return it.next().cloned();
        }
        if let Some(name) = a.strip_prefix("--package=") {
            return Some(name.to_string());
        }
    }
    None
}

/// The resolve of the program under the cargo flags that select what it builds.
///
/// # Errors
/// Cargo fails.
pub fn cargo_metadata(crate_dir: &Path, cargo_args: &[String]) -> Result<Value, String> {
    let mut command = std::process::Command::new("cargo");
    command
        .current_dir(crate_dir)
        .args(["metadata", "--format-version", "1"]);
    let mut it = cargo_args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest-path" | "--features" | "-F" | "--config" | "-Z" => {
                command.arg(a).arg(it.next().cloned().unwrap_or_default());
            }
            "--no-default-features" | "--all-features" | "--locked" | "--offline" | "--frozen" => {
                command.arg(a);
            }
            s if s.starts_with("--manifest-path=")
                || s.starts_with("--features=")
                || s.starts_with("--config=") =>
            {
                command.arg(s);
            }
            _ => {}
        }
    }
    command.args(["--filter-platform", "mipsel-sony-psx"]);
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn register_hit(s: &str) -> bool {
        Regex::new(REGISTER).unwrap().is_match(s)
    }

    #[test]
    fn comments_are_not_code() {
        let lines = code_lines(
            "let a = 1; // 0x1f80_1810\n/* 0x1f80\n 0x1f80 */ let b = 0x1f80;\n\"x // y 0x1f80\"",
        );
        assert!(!register_hit(&lines[0]) && !register_hit(&lines[1]));
        assert!(register_hit(&lines[2]) && register_hit(&lines[3]));
    }

    #[test]
    fn patterns() {
        assert!(
            register_hit("0x1F80_1814") && register_hit("0xBF801810") && !register_hit("0x1234")
        );
        let v = Regex::new(VECTOR).unwrap();
        assert!(
            v.is_match("0x8000_0080") && v.is_match("0xA0000080") && !v.is_match("0x8000_0100")
        );
        let c = Regex::new(COP0).unwrap();
        assert!(c.is_match("\"mfc0 $2, $12\"") && c.is_match("rfe") && !c.is_match("surfer"));
    }

    /// A scratch workspace: a program package `game` whose source is `source`, linked to an SDK
    /// at `<dir>/sdk-root` holding `psx-rt` (features `runtime`, `host-tty`, `alloc`) built with
    /// the `enabled` ones, plus any extra packages (name, dir relative to the scratch dir).
    struct Fixture {
        dir: PathBuf,
        metadata: Value,
    }

    fn fixture(name: &str, source: &str, enabled: &[&str], extra: &[(&str, &str)]) -> Fixture {
        fixture_at(name, "game", source, enabled, extra)
    }

    /// The same with the program inside the SDK repository (`sdk-root`).
    fn local_fixture(name: &str, source: &str, enabled: &[&str]) -> Fixture {
        fixture_at(name, "sdk-root/engine/examples/game", source, enabled, &[])
    }

    fn fixture_at(
        name: &str,
        game: &str,
        source: &str,
        enabled: &[&str],
        extra: &[(&str, &str)],
    ) -> Fixture {
        let dir = std::env::temp_dir().join(format!("psx-gate-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(game).join("src")).unwrap();
        fs::write(dir.join(game).join("src/lib.rs"), source).unwrap();
        let sdk = dir.join("sdk-root/sdk/crates/psx-rt");
        fs::create_dir_all(sdk.join("src")).unwrap();
        fs::write(sdk.join("src/lib.rs"), "const A: u32 = 0x1f80_1810;\n").unwrap();
        let canon = |p: PathBuf| p.canonicalize().unwrap().display().to_string();
        let mut packages = vec![
            json!({"id": "game", "name": "game", "source": null,
                   "manifest_path": format!("{}/Cargo.toml", canon(dir.join(game))),
                   "features": {},
                   "targets": [{"kind": ["lib"], "src_path": format!("{}/src/lib.rs", canon(dir.join(game)))}]}),
            json!({"id": "psx-rt", "name": "psx-rt", "source": null,
                   "manifest_path": format!("{}/Cargo.toml", canon(sdk.clone())),
                   "features": {"runtime": [], "host-tty": [], "alloc": [], "default": ["runtime"]},
                   "targets": [{"kind": ["lib"], "src_path": format!("{}/src/lib.rs", canon(sdk.clone()))}]}),
        ];
        let mut deps = vec![json!({"pkg": "psx-rt", "dep_kinds": [{"kind": null}]})];
        let mut nodes = vec![json!({"id": "psx-rt", "deps": [], "features": enabled})];
        for (extra_name, rel) in extra {
            fs::create_dir_all(dir.join(rel).join("src")).unwrap();
            fs::write(dir.join(rel).join("src/lib.rs"), "").unwrap();
            packages.push(json!({"id": extra_name, "name": extra_name, "source": null,
                "manifest_path": format!("{}/Cargo.toml", canon(dir.join(rel))),
                "features": {}, "targets": [{"kind": ["lib"], "src_path": format!("{}/src/lib.rs", canon(dir.join(rel)))}]}));
            deps.push(json!({"pkg": extra_name, "dep_kinds": [{"kind": null}]}));
            nodes.push(json!({"id": extra_name, "deps": [], "features": []}));
        }
        nodes.push(json!({"id": "game", "deps": deps, "features": []}));
        let metadata = json!({"packages": packages, "workspace_root": canon(dir.join(game)),
            "resolve": {"root": "game", "nodes": nodes}});
        Fixture { dir, metadata }
    }

    fn profile(extra: &str) -> Profile {
        let text =
            format!("[shipping]\npsx-rt = [\"runtime\"]\n[diag]\npsx-rt = [\"host-tty\"]\n{extra}");
        Profile::parse(&text, Path::new("p")).unwrap()
    }

    const NOTHING: &str = "";

    /// Check the fixture program with `own` as its `psx-gate.toml` text (a program that is not
    /// inside the SDK repository lists its exceptions there).
    fn run_check(f: &Fixture, profile: &Profile, own: &str) -> Outcome {
        let own = Profile::parse_program_file(own, Path::new("psx-gate.toml")).unwrap();
        check(
            &f.metadata,
            None,
            None,
            &f.dir.join("sdk-root"),
            profile,
            Some(&own),
        )
        .unwrap()
    }

    fn exception(kind: &str, item: &str, count: Option<u32>) -> String {
        let count = count.map(|n| format!("count = {n}\n")).unwrap_or_default();
        format!(
            "[[exception]]\nprogram = \"game\"\nkind = \"{kind}\"\nitem = \"{item}\"\n{count}remove = \"W9\"\n"
        )
    }

    #[test]
    fn a_clean_program_passes_and_the_sdk_is_not_scanned() {
        let f = fixture("clean", "pub fn f() {}\n", &["runtime"], &[]);
        let out = run_check(&f, &profile(""), NOTHING);
        assert!(out.violations.is_empty(), "{:?}", out.violations);
        assert!(!out.diag);
    }

    #[test]
    fn a_register_literal_is_refused_with_file_line_rule_and_remedy() {
        let f = fixture(
            "reg",
            "fn f() {\n    let p = 0x1f80_1814 as *mut u32;\n}\n",
            &["runtime"],
            &[],
        );
        let out = run_check(&f, &profile(""), NOTHING);
        assert_eq!(out.violations.len(), 1);
        let v = &out.violations[0];
        assert!(
            v.contains("src/lib.rs:2") && v.contains("register-literal"),
            "{v}"
        );
        assert!(
            v.contains("sdk/docs/GATE.md rule 3") && v.contains("psx_hw"),
            "{v}"
        );
    }

    #[test]
    fn vector_writes_and_cop0_assembly_are_refused() {
        let src = "fn f() {\n    let v = 0x8000_0080;\n    asm!(\"mfc0 $8, $12\");\n}\n";
        let f = fixture("vec", src, &["runtime"], &[]);
        let out = run_check(&f, &profile(""), NOTHING);
        let kinds: Vec<&str> = out.proposed.iter().map(|e| e.kind.as_str()).collect();
        assert!(
            kinds.contains(&"vector-write") && kinds.contains(&"cop0-asm"),
            "{kinds:?}"
        );
        assert_eq!(out.violations.len(), 2);
    }

    #[test]
    fn a_listed_count_passes_a_higher_one_names_the_lines_and_a_lower_one_must_shrink() {
        let src = "const A: u32 = 0x1f80_1810;\nconst B: u32 = 0x1f80_1814;\n";
        let f = fixture("count", src, &["runtime"], &[]);
        let key = "src/lib.rs";
        let p = profile("");
        let exact = exception("register-literal", key, Some(2));
        assert!(run_check(&f, &p, &exact).violations.is_empty());
        let one = exception("register-literal", key, Some(1));
        let out = run_check(&f, &p, &one);
        assert_eq!(out.violations.len(), 1);
        assert!(
            out.violations[0].contains("2 register-literal lines")
                && out.violations[0].contains("lists 1")
                && out.violations[0].contains("psx-gate.toml")
                && out.violations[0].contains("lib.rs:1")
                && out.violations[0].contains("lib.rs:2")
        );
        let three = exception("register-literal", key, Some(3));
        let out = run_check(&f, &p, &three);
        assert!(
            out.violations[0].contains("lower the count to 2"),
            "{:?}",
            out.violations
        );
    }

    #[test]
    fn an_entry_that_matches_nothing_fails() {
        let f = fixture("stale", "pub fn f() {}\n", &["runtime"], &[]);
        let stale = exception("register-literal", "src/lib.rs", Some(1));
        let out = run_check(&f, &profile(""), &stale);
        assert_eq!(out.violations.len(), 1);
        assert!(
            out.violations[0].contains("delete the entry"),
            "{:?}",
            out.violations
        );
        // Another program's entry in a shared file is not this program's business.
        let other = stale.replace("\"game\"", "\"other\"");
        assert!(run_check(&f, &profile(""), &other).violations.is_empty());
    }

    #[test]
    fn features_shipping_diag_excepted_and_unlisted() {
        let f = fixture(
            "feat",
            "pub fn f() {}\n",
            &["runtime", "host-tty", "alloc"],
            &[],
        );
        let out = run_check(&f, &profile(""), NOTHING);
        assert!(out.diag && out.diag_features == ["psx-rt/host-tty"]);
        assert_eq!(out.violations.len(), 1);
        assert!(out.violations[0].contains("psx-rt/alloc") && out.violations[0].contains("rule 2"));
        let excepted = exception("feature", "psx-rt/alloc", None);
        assert!(run_check(&f, &profile(""), &excepted).violations.is_empty());
        // The program stopped enabling it: the entry is stale.
        let without = fixture("feat2", "pub fn f() {}\n", &["runtime"], &[]);
        let out = run_check(&without, &profile(""), &excepted);
        assert!(out.violations[0].contains("delete the entry"));
    }

    #[test]
    fn a_private_copy_of_an_sdk_crate_is_refused() {
        let f = fixture(
            "copy",
            "pub fn f() {}\n",
            &["runtime"],
            &[("psx-chainloader", "vendor/psx-chainloader")],
        );
        let out = run_check(&f, &profile(""), NOTHING);
        assert_eq!(out.violations.len(), 1);
        assert!(out.violations[0].contains("private-copy") && out.violations[0].contains("rule"));
        let listed = exception("private-copy", "psx-chainloader", None);
        assert!(run_check(&f, &profile(""), &listed).violations.is_empty());
    }

    #[test]
    fn the_sdk_profile_lists_only_programs_inside_the_sdk_repository() {
        // A program outside the SDK repository: the SDK's profile does not speak for it.
        let src = "const A: u32 = 0x1f80_1810;\n";
        let outside = fixture("outside", src, &["runtime"], &[]);
        let sdk_lists_it = profile(&exception("register-literal", "src/lib.rs", Some(1)));
        assert!(!run_check(&outside, &sdk_lists_it, NOTHING)
            .violations
            .is_empty());
        // One inside it: its entries are the SDK's, and the program's own file is not read.
        let inside = local_fixture("inside", src, &["runtime"]);
        let out = run_check(&inside, &sdk_lists_it, NOTHING);
        assert!(out.sdk_local && out.program == "game" && out.violations.is_empty());
        assert!(!run_check(&inside, &profile(""), NOTHING)
            .violations
            .is_empty());
    }

    #[test]
    fn a_probe_program_is_exempt_from_the_line_rules_and_nothing_else() {
        let src = "const A: u32 = 0x1f80_1810;\nconst V: u32 = 0x8000_0080;\nfn f() { asm!(\"mfc0 $2, $12\"); }\n";
        let probe = profile("[probe]\nprograms = [\"game\"]\n");
        let inside = local_fixture("probe", src, &["runtime", "alloc"]);
        let out = run_check(&inside, &probe, NOTHING);
        // The three line rules are not applied; the unlisted feature still is.
        assert_eq!(out.violations.len(), 1, "{:?}", out.violations);
        assert!(out.violations[0].contains("psx-rt/alloc"));
        // A stale entry for an exempt kind is still refused (counted entries are deleted).
        let stale = profile(
            "[probe]\nprograms = [\"game\"]\n[[exception]]\nprogram = \"game\"\nkind = \"register-literal\"\nitem = \"src/lib.rs\"\ncount = 1\nremove = \"decision\"\n",
        );
        let out = run_check(&inside, &stale, NOTHING);
        assert!(out
            .violations
            .iter()
            .any(|v| v.contains("delete the entry")));
        // The same program outside the SDK repository is not a probe.
        let outside = fixture("probe-out", src, &["runtime"], &[]);
        assert_eq!(run_check(&outside, &probe, NOTHING).violations.len(), 3);
    }

    #[test]
    fn a_deprecated_symbol_is_refused_counted_and_ratcheted() {
        let src = "fn f() { let a = poll_old(); let b = OldReader::new(); poll_old_diag(); }\n";
        let f = fixture("deprecated", src, &["runtime"], &[]);
        let with = profile("[deprecated]\nsymbols = [\"poll_old\", \"OldReader\"]\n");
        let out = run_check(&f, &with, NOTHING);
        assert_eq!(out.violations.len(), 1, "{:?}", out.violations);
        assert!(out.violations[0].contains("deprecated-use"));
        let listed = exception("deprecated-use", "src/lib.rs", Some(1));
        assert!(run_check(&f, &with, &listed).violations.is_empty());
        // No list of symbols: nothing to find.
        assert!(run_check(&f, &profile(""), NOTHING).violations.is_empty());
    }
}
