//! What part of the SDK one game can reach, so the pin rule (pin.rs) compares only that.
//!
//! Over every path in [`GUEST_PATHS`], a change to a disc no game links (the hardware tests
//! under `engine/examples/hardware-tests`) moves the guest tree, and every game has to repin
//! before it can land again. A game's scope leaves out the SDK crates the game cannot reach.
//! Files outside every crate directory (linker scripts, workspace manifests and locks, the
//! toolchain file, the guest tool scripts) and the guest tool crates always count.
//!
//! A game reaches a crate when:
//! - one of its manifests has a git dependency on the SDK naming the crate's package;
//! - one of its files names the crate's directory (a `.psoxide/sdk/crates/psx-rt` path
//!   dependency, `--manifest-path .psoxide/engine/examples/editor-playtest/Cargo.toml`), a file
//!   in it, or a directory above it (`sdk/examples`), or composes the name at run time
//!   (`sdk/examples/$(EXAMPLE)`), or passes its package to cargo (`-p NAME`);
//! - a guest tool script the game names (`tools/sdk-examples.mk`) does one of these;
//! - a crate it reaches depends on it by path, directly or through its workspace.
//!
//! An asset file (`assets/audio`, `assets/textures`) counts for a game that names it (its lock
//! hydrates the directory, or a file mentions the path) or whose reached crate reads it
//! (`include_bytes!("../../../../assets/textures/floor.psxt")`); any other asset counts for no one.
//!
//! Documentation (`*.md`, `docs/`) and the pin locks (`components.lock.json`, `sdk.lock.json`:
//! they list what is hydrated, not what is built) are not read, except for the asset paths in
//! a lock. Whenever something cannot be
//! read with certainty (no git work tree, a manifest that does not parse, a path dependency
//! outside the repository, a git dependency on a package the SDK does not have, a workspace
//! dependency without its workspace), the scope is the whole guest tree, as before.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::pin::GUEST_PATHS;
use crate::util;

/// What a game's own files say it takes from the SDK.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reach {
    /// Packages of git dependencies on the SDK: each must exist in the SDK.
    pub git_packages: BTreeSet<String>,
    /// Packages passed to cargo (`-p NAME`); names that are not SDK packages are the game's own.
    pub cargo_packages: BTreeSet<String>,
    /// SDK paths the files name (`sdk/crates/psx-rt`, `engine/examples/editor-playtest/src`).
    pub mentions: BTreeSet<String>,
    /// SDK directories whose children the files name through a variable.
    pub composed: BTreeSet<String>,
    /// Guest tool scripts (files of [`GUEST_PATHS`]) the files name.
    pub tools: BTreeSet<String>,
    /// SDK manifests named on a line with no `-p` (`""` is the root's): a workspace among
    /// them is built whole.
    pub workspace_builds: BTreeSet<String>,
    /// Asset paths (`assets/audio`, `assets/textures/floor.psxt`) the files name, the pin locks
    /// included: the game hydrates them or builds from them.
    pub assets: BTreeSet<String>,
    /// The backstop: SDK packages the game's resolved dependencies link (from its Cargo.lock
    /// files: path packages that are not the game's own, and git packages from the SDK). Each
    /// one the SDK has must be reached, or the scan missed a way in and the tree is whole.
    pub linked: BTreeSet<String>,
    /// The same as SDK directories, where `cargo metadata` gave paths (a manifest with no lock).
    pub linked_dirs: BTreeSet<String>,
}

impl Reach {
    /// One line for the cache key: two equal reaches give equal scoped trees.
    pub fn digest(&self) -> String {
        let mut key = util::Key::new("sdk-scope");
        let sets = [
            ("git", &self.git_packages),
            ("cargo", &self.cargo_packages),
            ("mention", &self.mentions),
            ("composed", &self.composed),
            ("tool", &self.tools),
            ("workspace", &self.workspace_builds),
            ("asset", &self.assets),
            ("linked", &self.linked),
            ("linked-dir", &self.linked_dirs),
        ];
        for (label, set) in sets {
            for item in set {
                key.part(label, item);
            }
        }
        key.finish()
    }
}

/// The SDK a game is judged against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every guest path, with the reason the game's reach is not known.
    Whole(String),
    /// What the game reaches.
    Game(Reach),
}

/// The scope of the game in the git work tree `repo`.
pub fn of_game(repo: &Path) -> Scope {
    let files = match util::tree_files(repo) {
        Ok(files) => files,
        Err(e) => return Scope::Whole(format!("cannot list the game's files: {e}")),
    };
    let mut texts = Vec::new();
    for rel in files {
        if skipped(&rel) {
            continue;
        }
        // Binary files (and files that are gone from the work tree) name no SDK path.
        if let Ok(text) = std::fs::read_to_string(repo.join(&rel)) {
            texts.push((rel, text));
        }
    }
    let mut reach = match reach_of(&texts) {
        Ok(reach) => reach,
        Err(e) => return Scope::Whole(e),
    };
    match linked(repo, &texts) {
        Ok((names, dirs)) => {
            reach.linked = names;
            reach.linked_dirs = dirs;
            Scope::Game(reach)
        }
        Err(e) => Scope::Whole(e),
    }
}

/// What the game's resolved dependency graphs link from the SDK, for the backstop: package
/// names from every Cargo.lock that covers one of its manifests, and SDK directories from
/// `cargo metadata --offline` for a manifest no lock covers. A manifest neither can read
/// is an error.
fn linked(
    repo: &Path,
    files: &[(String, String)],
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let manifests: Vec<&(String, String)> = files
        .iter()
        .filter(|(rel, _)| rel == "Cargo.toml" || rel.ends_with("/Cargo.toml"))
        .filter(|(rel, _)| !skipped(rel))
        .collect();
    let own: BTreeSet<String> = manifests
        .iter()
        .filter_map(|(_, text)| {
            let table: toml::Table = toml::from_str(text).ok()?;
            Some(table.get("package")?.get("name")?.as_str()?.to_string())
        })
        .collect();
    let mut locks: BTreeMap<String, Option<Vec<LockPackage>>> = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    for (rel, text) in manifests {
        let dir = rel
            .strip_suffix("Cargo.toml")
            .unwrap_or_default()
            .trim_end_matches('/');
        let package = toml::from_str::<toml::Table>(text)
            .ok()
            .and_then(|t| Some(t.get("package")?.get("name")?.as_str()?.to_string()));
        // The nearest lock above the manifest that lists it (its workspace's).
        let mut covering = None;
        let mut at = Some(dir);
        while let Some(d) = at {
            let lock = if d.is_empty() {
                "Cargo.lock".to_string()
            } else {
                format!("{d}/Cargo.lock")
            };
            let packages = locks
                .entry(lock.clone())
                .or_insert_with(|| read_lock(&repo.join(&lock)))
                .as_ref();
            if let Some(packages) = packages {
                let lists_it = match &package {
                    Some(name) => packages
                        .iter()
                        .any(|p| p.name == *name && p.source.is_none()),
                    // A virtual manifest's lock sits beside it.
                    None => d == dir,
                };
                if lists_it {
                    covering = Some(lock);
                    break;
                }
            }
            at = match d.rsplit_once('/') {
                Some((parent, _)) => Some(parent),
                None if !d.is_empty() => Some(""),
                None => None,
            };
        }
        match covering.and_then(|lock| locks.get(&lock)?.as_ref()) {
            Some(packages) => {
                for p in packages {
                    match &p.source {
                        None if !own.contains(&p.name) => {
                            names.insert(p.name.clone());
                        }
                        Some(source)
                            if source.starts_with("git+")
                                && is_sdk_repository(
                                    source
                                        .trim_start_matches("git+")
                                        .split(['?', '#'])
                                        .next()
                                        .unwrap_or(""),
                                ) =>
                        {
                            names.insert(p.name.clone());
                        }
                        _ => {}
                    }
                }
            }
            None => dirs.extend(metadata_dirs(repo, rel)?),
        }
    }
    Ok((names, dirs))
}

/// One `[[package]]` of a Cargo.lock.
#[derive(Debug)]
struct LockPackage {
    name: String,
    source: Option<String>,
}

/// The packages of the Cargo.lock at `path`, or `None` when there is none to read.
fn read_lock(path: &Path) -> Option<Vec<LockPackage>> {
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some(
        table
            .get("package")?
            .as_array()?
            .iter()
            .filter_map(|p| {
                Some(LockPackage {
                    name: p.get("name")?.as_str()?.to_string(),
                    source: p
                        .get("source")
                        .and_then(toml::Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect(),
    )
}

/// The SDK directories (`sdk/crates/psx-rt`) of the hydrated `.psoxide/` packages in the
/// resolved graph of the manifest `rel`, from `cargo metadata --offline`.
fn metadata_dirs(repo: &Path, rel: &str) -> Result<BTreeSet<String>, String> {
    let fail = |why: String| {
        format!("{rel}: no Cargo.lock lists it and cargo metadata cannot read it: {why}")
    };
    let out = std::process::Command::new("cargo")
        .current_dir(repo)
        .args([
            "metadata",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            rel,
        ])
        .output()
        .map_err(|e| fail(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(fail(err.lines().last().unwrap_or("").trim().to_string()));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| fail(e.to_string()))?;
    let root = repo.canonicalize().map_err(|e| fail(e.to_string()))?;
    let mut dirs = BTreeSet::new();
    for package in json["packages"].as_array().into_iter().flatten() {
        let Some(manifest) = package["manifest_path"].as_str() else {
            continue;
        };
        let manifest = Path::new(manifest);
        let manifest = manifest
            .canonicalize()
            .unwrap_or_else(|_| manifest.to_path_buf());
        if let Ok(inside) = manifest.strip_prefix(&root) {
            let inside = inside.display().to_string();
            if let Some(sdk) = inside.strip_prefix(".psoxide/") {
                dirs.insert(
                    sdk.trim_end_matches("Cargo.toml")
                        .trim_end_matches('/')
                        .to_string(),
                );
            }
        }
    }
    Ok(dirs)
}

/// Files that cannot build anything: documentation, the pin locks, the hydrated SDK itself.
fn skipped(rel: &str) -> bool {
    crate::pin::is_doc(rel)
        || rel == "components.lock.json"
        || rel == "sdk.lock.json"
        || rel.split('/').any(|c| c == ".psoxide")
}

/// The reach of a game from its files (path relative to the repository, text).
pub fn reach_of(files: &[(String, String)]) -> Result<Reach, String> {
    let mut reach = Reach::default();
    for (rel, text) in files {
        if skipped(rel) {
            // A lock lists what the game hydrates: the asset directories it takes count.
            if rel == "components.lock.json" {
                text.lines().for_each(|line| scan_assets(line, &mut reach));
            }
            continue;
        }
        scan_text(text, &mut reach);
        if rel == "Cargo.toml" || rel.ends_with("/Cargo.toml") {
            let dir = rel.strip_suffix("Cargo.toml").unwrap_or_default();
            scan_game_manifest(dir.trim_end_matches('/'), text, &mut reach)
                .map_err(|e| format!("{rel}: {e}"))?;
        }
    }
    Ok(reach)
}

/// The first segments of every SDK path a file can name.
const PREFIXES: [&str; 4] = ["sdk/", "engine/", "crates/", "tools/"];

/// Characters of a path as files write it.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')
}

/// Characters that start a name made at run time (`$(X)`, `${x}`, `{name}`, `*`, `<name>`, `%`).
fn is_composition(c: char) -> bool {
    matches!(c, '$' | '{' | '*' | '<' | '%')
}

/// Add what `text` names of the SDK to `reach`.
fn scan_text(text: &str, reach: &mut Reach) {
    for line in text.lines() {
        scan_line(line, reach);
    }
    for tool in guest_tool_files() {
        let base = tool.rsplit('/').next().unwrap_or(tool);
        if text.contains(base) {
            reach.tools.insert(tool.to_string());
        }
    }
}

/// One line of [`scan_text`] (a path and a cargo command's arguments sit on one line).
fn scan_line(line: &str, reach: &mut Reach) {
    scan_assets(line, reach);
    let mut words = line
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '[' | ']' | '='))
        .filter(|w| !w.is_empty());
    let mut packages = false;
    while let Some(word) = words.next() {
        if matches!(word, "-p" | "--package" | "--bin" | "--example") {
            if let Some(name) = words.next() {
                reach.cargo_packages.insert(name.to_string());
                packages = true;
            }
        }
    }
    let mut manifests = Vec::new();
    for prefix in PREFIXES {
        for (at, _) in line.match_indices(prefix) {
            // `../.psoxide/sdk/...` and `"sdk/...` start a path; `mysdk/` and `my-sdk/` do not.
            if line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                continue;
            }
            let run: String = line[at..]
                .chars()
                .take_while(|&c| is_path_char(c))
                .collect();
            let next = line[at + run.len()..].chars().next();
            if next.is_some_and(is_composition) {
                if let Some((parent, _)) = run.rsplit_once('/') {
                    reach.composed.insert(parent.to_string());
                }
            }
            let path = run.trim_end_matches(['/', '.']);
            if let Some(dir) = path.strip_suffix("/Cargo.toml") {
                manifests.push(dir.to_string());
            }
            // A bare top-level directory (`engine/`, "sdk/, crates/ and engine/") is prose or a
            // copy of the tree, not a build of one crate: a build names a crate, a manifest,
            // a package, or composes a name.
            if path.contains('/') {
                reach.mentions.insert(path.to_string());
            }
        }
    }
    // The SDK's root manifest (`.psoxide/Cargo.toml`, `$(PSOXIDE)/Cargo.toml`).
    for (at, _) in line.match_indices("Cargo.toml") {
        let before: String = line[..at]
            .chars()
            .rev()
            .take_while(|&c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '='))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let last = before
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        if before.ends_with('/') && last.to_ascii_lowercase().contains("psoxide") {
            manifests.push(String::new());
        }
    }
    // Without `-p`, cargo builds what the manifest names: for a workspace, its members.
    if !packages {
        reach.workspace_builds.extend(manifests);
    }
}

/// Add the asset paths (`assets/audio/cdda`) a line names to `reach`.
fn scan_assets(line: &str, reach: &mut Reach) {
    for (at, _) in line.match_indices("assets/") {
        // `my-assets/` and `x.assets/` are other names; `../.psoxide/assets/` and `"assets/` are not.
        if line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            continue;
        }
        let run: String = line[at..]
            .chars()
            .take_while(|&c| is_path_char(c))
            .collect();
        let path = run.trim_end_matches(['/', '.']);
        if path.len() > "assets/".len() {
            reach.assets.insert(path.to_string());
        }
    }
}

/// The guest paths that are tool scripts rather than directories (`tools/sdk-examples.mk`).
fn guest_tool_files() -> impl Iterator<Item = &'static str> {
    GUEST_PATHS.iter().copied().filter(|p| {
        p.starts_with("tools/") && p.rsplit('/').next().is_some_and(|b| b.contains('.'))
    })
}

/// `dir/rel` with `.` and `..` resolved, or `None` when it climbs above the root.
fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split('/').chain(rel.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Every `path = "..."` and SDK git dependency of a game manifest. The SDK paths themselves
/// are already mentions of the text; this checks that none leaves the repository and reads
/// the git dependencies' packages.
fn scan_game_manifest(dir: &str, text: &str, reach: &mut Reach) -> Result<(), String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut error = None;
    walk(&table, &mut |key, entry| {
        if let Some(rel) = entry.get("path").and_then(toml::Value::as_str) {
            if join(dir, rel).is_none() && error.is_none() {
                error = Some(format!(
                    "the path dependency {rel} is outside the repository"
                ));
            }
        }
        if let Some(url) = entry.get("git").and_then(toml::Value::as_str) {
            if is_sdk_repository(url) {
                let name = entry
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                reach.git_packages.insert(name.to_string());
            }
        }
    });
    error.map_or(Ok(()), Err)
}

/// Whether a git URL is the SDK's repository (not the emulator's or the editor's).
fn is_sdk_repository(url: &str) -> bool {
    let slug = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>();
    matches!(slug.as_slice(), [name, owner] if name.eq_ignore_ascii_case("PSoXide")
        && owner.eq_ignore_ascii_case("EBonura"))
}

/// Call `visit(key, table)` for every table nested in `table` (array elements by their key).
fn walk(table: &toml::Table, visit: &mut impl FnMut(&str, &toml::Table)) {
    fn value(key: &str, v: &toml::Value, visit: &mut impl FnMut(&str, &toml::Table)) {
        match v {
            toml::Value::Table(t) => {
                visit(key, t);
                walk(t, visit);
            }
            toml::Value::Array(items) => {
                for item in items {
                    value(key, item, visit);
                }
            }
            _ => {}
        }
    }
    for (key, v) in table {
        value(key, v, visit);
    }
}

/// One SDK manifest, read for the closure.
struct Manifest {
    table: toml::Table,
}

impl Manifest {
    fn package(&self) -> Option<&str> {
        self.table.get("package")?.get("name")?.as_str()
    }

    fn is_workspace(&self) -> bool {
        self.table.contains_key("workspace")
    }
}

/// Whether a mention, path or directory `m` names the crate directory `dir`: the directory
/// itself, a file inside it, or a directory above it.
fn names(m: &str, dir: &str) -> bool {
    m == dir
        || m.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
        || dir.strip_prefix(m).is_some_and(|r| r.starts_with('/'))
}

/// The crate directories of one SDK commit, and those of them a game reaches.
#[derive(Debug, Default)]
pub struct Reached {
    /// Every directory whose manifest has a `[package]`.
    pub crates: BTreeSet<String>,
    /// The ones the game reaches.
    pub reached: BTreeSet<String>,
    /// Asset paths the game names, and those the sources of the crates it reaches read.
    pub assets: BTreeSet<String>,
}

impl Reached {
    /// Whether `path` counts: an asset the game names or a reached crate reads, or a file in no crate
    /// directory, or in one the game reaches.
    pub fn keeps(&self, path: &str) -> bool {
        // An asset counts when a reached crate reads it or the game names it (or a directory
        // above it); every other asset is read by nothing the game builds.
        if path.starts_with("assets/") {
            return self.assets.iter().any(|a| {
                path == a
                    || path
                        .strip_prefix(a.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            });
        }
        let mut inside = self
            .crates
            .iter()
            .filter(|dir| {
                path.strip_prefix(dir.as_str())
                    .is_some_and(|r| r.starts_with('/'))
            })
            .peekable();
        inside.peek().is_none() || inside.any(|dir| self.reached.contains(dir))
    }
}

/// The Rust sources of some crate directories at one commit: `(path, text)` of every `.rs`
/// file in them (a nested crate's files may be included or not).
pub type Sources<'a> = dyn FnMut(&[String]) -> Result<Vec<(String, String)>, String> + 'a;

/// The crate directories a game reaches, given every SDK manifest of the guest paths at one
/// commit (directory of the manifest, `""` for the root -> its text), the texts of the guest
/// tool scripts the game names, and a way to read the reached crates' sources (a crate that
/// reads another crate's file, `include_str!("../../psx-cdstream/src/hw.rs")`, reaches it).
pub fn reached_crates(
    manifests: &BTreeMap<String, String>,
    tool_texts: &BTreeMap<String, String>,
    reach: &Reach,
    sources: &mut Sources<'_>,
) -> Result<Reached, String> {
    let mut parsed = BTreeMap::new();
    for (dir, text) in manifests {
        let table: toml::Table =
            toml::from_str(text).map_err(|e| format!("{dir}/Cargo.toml: {e}"))?;
        parsed.insert(dir.clone(), Manifest { table });
    }
    let crates: BTreeMap<&str, &str> = parsed
        .iter()
        .filter_map(|(dir, m)| Some((dir.as_str(), m.package()?)))
        .collect();

    // The game's reach and that of the tool scripts it names.
    let mut reach = reach.clone();
    for tool in reach.tools.clone() {
        let text = tool_texts
            .get(&tool)
            .ok_or_else(|| format!("the game names {tool}, which this SDK commit lacks"))?;
        scan_text(text, &mut reach);
    }

    let mut stack: Vec<String> = Vec::new();
    for (&dir, &name) in &crates {
        let always = GUEST_PATHS
            .iter()
            .any(|g| g.starts_with("tools/") && names(g, dir));
        let named = reach.mentions.iter().any(|m| names(m, dir))
            || reach.composed.iter().any(|p| names(p, dir))
            || reach.cargo_packages.contains(name);
        if always || named {
            stack.push(dir.to_string());
        }
    }
    for dir in &reach.workspace_builds {
        let Some(members) = parsed
            .get(dir)
            .and_then(|m| m.table.get("workspace")?.get("members")?.as_array())
        else {
            continue;
        };
        for member in members.iter().filter_map(toml::Value::as_str) {
            let path = join(dir, member)
                .ok_or_else(|| format!("{dir}/Cargo.toml: the member {member} leaves the SDK"))?;
            match path.split_once('*') {
                Some((parent, _)) => {
                    let parent = parent.trim_end_matches('/');
                    stack.extend(
                        crates
                            .keys()
                            .filter(|d| names(parent, d) && **d != parent)
                            .map(|d| d.to_string()),
                    );
                }
                None => stack.push(path),
            }
        }
    }
    for package in &reach.git_packages {
        let dir = crates
            .iter()
            .find(|(_, &name)| name == package)
            .map(|(&dir, _)| dir)
            .ok_or_else(|| {
                format!("the game takes {package} from the SDK by git, which has no such package")
            })?;
        stack.push(dir.to_string());
    }

    let all: BTreeSet<String> = crates.keys().map(|d| d.to_string()).collect();
    let mut seen = BTreeSet::new();
    let mut assets = reach.assets.clone();
    // In waves, so each wave's sources are read in one batch.
    while !stack.is_empty() {
        let wave: Vec<String> = std::mem::take(&mut stack)
            .into_iter()
            .filter(|dir| seen.insert(dir.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if wave.is_empty() {
            break;
        }
        for dir in &wave {
            if let Some(manifest) = parsed.get(dir) {
                stack.extend(
                    dependencies(dir, manifest, &parsed)?
                        .into_iter()
                        .filter(|t| crates.contains_key(t.as_str())),
                );
            }
        }
        for (path, text) in sources(&wave)? {
            let owner = owner_of(&all, &path);
            let file_dir = path.rsplit_once('/').map_or("", |(d, _)| d);
            for target in relative_reads(&text, file_dir, owner.unwrap_or(file_dir)) {
                if target.starts_with("assets/") {
                    assets.insert(target.clone());
                }
                if let Some(read) = owner_of(&all, &target) {
                    stack.push(read.to_string());
                }
            }
        }
    }
    // The backstop: what the game's lock files link from the SDK, the scan must have reached.
    for name in &reach.linked {
        let dirs: Vec<&str> = crates
            .iter()
            .filter(|(_, n)| **n == name.as_str())
            .map(|(d, _)| *d)
            .collect();
        if !dirs.is_empty() && !dirs.iter().any(|d| seen.contains(*d)) {
            return Err(format!(
                "the game links {name} ({}) but its files do not show how",
                dirs.join(", ")
            ));
        }
    }
    for dir in &reach.linked_dirs {
        if all.contains(dir) && !seen.contains(dir) {
            return Err(format!(
                "the game links {dir} but its files do not show how"
            ));
        }
    }
    Ok(Reached {
        crates: all,
        reached: seen,
        assets,
    })
}

/// The innermost crate directory holding `path`.
fn owner_of<'a>(crates: &'a BTreeSet<String>, path: &str) -> Option<&'a str> {
    crates
        .iter()
        .filter(|dir| {
            path.strip_prefix(dir.as_str())
                .is_some_and(|r| r.starts_with('/'))
                || path == dir.as_str()
        })
        .max_by_key(|dir| dir.len())
        .map(String::as_str)
}

/// Where the relative path literals of a Rust file (`"../../x/y.rs"`, `"/../x"` after
/// `env!("CARGO_MANIFEST_DIR")`) lead, from the file's directory (`include_str!`) and from its
/// crate's (a build script, a test).
fn relative_reads(text: &str, file_dir: &str, crate_dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text
        .match_indices("\"../")
        .chain(text.match_indices("\"/../"))
    {
        let rest = &text[at + 1..];
        let literal = &rest[..rest.find('"').unwrap_or(rest.len())];
        for base in [file_dir, crate_dir] {
            if let Some(target) = join(base, literal) {
                out.push(target);
            }
        }
    }
    out
}

/// The SDK directories `dir`'s manifest points at by path: its own path entries, the
/// workspace entries of its `workspace = true` dependencies, and its workspace's patches.
fn dependencies(
    dir: &str,
    manifest: &Manifest,
    parsed: &BTreeMap<String, Manifest>,
) -> Result<Vec<String>, String> {
    let resolve = |base: &str, rel: &str| {
        join(base, rel).ok_or_else(|| format!("{dir}/Cargo.toml: {rel} leaves the SDK"))
    };
    let workspace = || {
        let mut at = dir;
        loop {
            at = match at.rsplit_once('/') {
                Some((parent, _)) => parent,
                None if !at.is_empty() => "",
                None => return None,
            };
            if let Some(m) = parsed.get(at).filter(|m| m.is_workspace()) {
                return Some((at, m));
            }
        }
    };
    let mut out = Vec::new();
    let mut inherited = Vec::new();
    let mut error = None;
    walk(&manifest.table, &mut |key, entry| {
        if let Some(rel) = entry.get("path").and_then(toml::Value::as_str) {
            match resolve(dir, rel) {
                Ok(p) => out.push(p),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        if entry.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
            inherited.push(key.to_string());
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    let own_workspace = manifest.is_workspace().then_some((dir, manifest));
    if let Some((ws_dir, ws)) = own_workspace.or_else(workspace) {
        let deps = ws
            .table
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .and_then(toml::Value::as_table);
        // `edition.workspace = true` and `[lints] workspace = true` inherit no dependency.
        for name in inherited
            .iter()
            .filter(|n| !matches!(n.as_str(), "lints" | "package"))
        {
            match deps.and_then(|d| d.get(name)) {
                Some(entry) => {
                    if let Some(rel) = entry.get("path").and_then(toml::Value::as_str) {
                        out.push(resolve(ws_dir, rel)?);
                    }
                }
                // A package field inherited (`version.workspace = true`) names no dependency.
                None if is_package_field(name) => {}
                None => {
                    return Err(format!(
                        "{dir}/Cargo.toml: {name} comes from the workspace at {ws_dir}, which does not list it"
                    ))
                }
            }
        }
        if let Some(patch) = ws.table.get("patch").and_then(toml::Value::as_table) {
            walk(patch, &mut |_, entry| {
                if let Some(rel) = entry.get("path").and_then(toml::Value::as_str) {
                    if let Ok(p) = resolve(ws_dir, rel) {
                        out.push(p);
                    }
                }
            });
        }
    } else if inherited
        .iter()
        .any(|n| !is_package_field(n) && n != "lints")
    {
        return Err(format!(
            "{dir}/Cargo.toml inherits from a workspace this SDK lacks"
        ));
    }
    Ok(out)
}

/// The `[package]` fields a manifest may take from its workspace (`edition.workspace = true`).
fn is_package_field(name: &str) -> bool {
    matches!(
        name,
        "version"
            | "authors"
            | "description"
            | "documentation"
            | "readme"
            | "homepage"
            | "repository"
            | "license"
            | "license-file"
            | "keywords"
            | "categories"
            | "publish"
            | "edition"
            | "rust-version"
            | "exclude"
            | "include"
            | "lints"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn a_game_reads_its_path_dependencies_and_commands() {
        let reach = reach_of(&files(&[
            (
                "game/Cargo.toml",
                "[package]\nname = \"g\"\n[dependencies]\n\
                 psx-rt = { path = \"../.psoxide/sdk/crates/psx-rt\" }\n\
                 link = { git = \"https://github.com/EBonura/PSoXide.git\", rev = \"abc\", package = \"psoxide-link\" }\n\
                 emu = { git = \"https://github.com/EBonura/PSoXide-emulator.git\" }\n",
            ),
            (
                "psoxide-test.toml",
                "commands = [[\"cargo\", \"run\", \"-p\", \"psx-vector-cook\"]]\n",
            ),
            ("Makefile", "\tmake -f $(PSOXIDE)/tools/sdk-examples.mk\n"),
            ("README.md", "see engine/examples/hardware-tests\n"),
            ("components.lock.json", "{\"paths\": [\"engine\"]}"),
        ]))
        .unwrap();
        assert!(reach.mentions.contains("sdk/crates/psx-rt"));
        assert!(reach.git_packages.contains("psoxide-link"));
        assert_eq!(reach.git_packages.len(), 1);
        assert!(reach.cargo_packages.contains("psx-vector-cook"));
        assert!(reach.tools.contains("tools/sdk-examples.mk"));
        // Neither the documentation nor the hydration list counts.
        assert!(!reach.mentions.iter().any(|m| m.starts_with("engine")));
    }

    #[test]
    fn prose_does_not_reach_a_tree_and_a_whole_workspace_build_reaches_its_members() {
        let mut reach = Reach::default();
        scan_text(
            "// the SDK (sdk/, crates/ and engine/) revision\n!docs/engine/\n",
            &mut reach,
        );
        assert!(reach.mentions.is_empty(), "{:?}", reach.mentions);
        scan_text(
            "cargo run --manifest-path .psoxide/Cargo.toml -p psoxide-pgo\n\
             cargo build --manifest-path $(PSOXIDE)/engine/Cargo.toml --workspace\n",
            &mut reach,
        );
        assert!(reach.cargo_packages.contains("psoxide-pgo"));
        assert_eq!(reach.workspace_builds, ["engine".to_string()].into());
        scan_text(
            "cargo build --manifest-path \"$(PSOXIDE)/Cargo.toml\"\n",
            &mut reach,
        );
        assert!(reach.workspace_builds.contains(""));
        let mut sdk = sdk();
        sdk.insert(
            "engine".into(),
            "[workspace]\nmembers = [\"crates/psx-engine\"]\n".into(),
        );
        sdk.insert(
            "engine/crates/psx-engine".into(),
            "[package]\nname = \"psx-engine\"\n".into(),
        );
        let reach = Reach {
            workspace_builds: ["engine".to_string()].into(),
            ..Reach::default()
        };
        let got = reached_crates(&sdk, &BTreeMap::new(), &reach, &mut no_sources)
            .unwrap()
            .reached;
        assert!(got.contains("engine/crates/psx-engine"));
        assert!(!got.contains("engine/examples/hardware-tests"));
    }

    #[test]
    fn a_linked_sdk_package_the_scan_missed_makes_the_tree_whole() {
        // The scan saw only psx-rt; the lock links psx-iso too.
        let mut reach = Reach {
            mentions: ["sdk/crates/psx-rt".to_string()].into(),
            linked: ["psx-rt".to_string(), "some-registry-crate".to_string()].into(),
            ..Reach::default()
        };
        assert!(reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources).is_ok());
        reach.linked.insert("psx-cook".to_string());
        let e = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources).unwrap_err();
        assert!(e.contains("psx-cook"), "{e}");
        let reach = Reach {
            linked_dirs: ["engine/examples/editor-playtest".to_string()].into(),
            ..Reach::default()
        };
        assert!(reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources).is_err());
    }

    /// A git work tree holding `files`.
    fn game_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        util::git(dir.path(), &["init", "-q"]).unwrap();
        util::git(dir.path(), &["add", "-A"]).unwrap();
        dir
    }

    #[test]
    fn the_backstop_reads_the_games_locks() {
        let manifest = "[package]\nname = \"g\"\n[dependencies]\n\
                        psx-rt = { path = \"../.psoxide/sdk/crates/psx-rt\" }\n";
        let lock = "version = 4\n[[package]]\nname = \"g\"\nversion = \"0.1.0\"\n\
                    [[package]]\nname = \"psx-rt\"\nversion = \"0.1.0\"\n\
                    [[package]]\nname = \"psx-iso\"\nversion = \"0.1.0\"\n\
                    source = \"git+ssh://git@github.com/EBonura/PSoXide.git?rev=abc#abc\"\n\
                    [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let repo = game_repo(&[("game/Cargo.toml", manifest), ("game/Cargo.lock", lock)]);
        let Scope::Game(reach) = of_game(repo.path()) else {
            panic!("a game with a lock has a scope");
        };
        assert_eq!(
            reach.linked,
            ["psx-iso".to_string(), "psx-rt".to_string()].into()
        );
        // No lock and nothing cargo can resolve offline: the whole tree.
        let repo = game_repo(&[("game/Cargo.toml", manifest)]);
        assert!(matches!(of_game(repo.path()), Scope::Whole(_)));
    }

    #[test]
    fn a_path_dependency_outside_the_repository_is_not_a_scope() {
        let e = reach_of(&files(&[(
            "Cargo.toml",
            "[dependencies]\npsx-rt = { path = \"../../PSoXide/sdk/crates/psx-rt\" }\n",
        )]))
        .unwrap_err();
        assert!(e.contains("outside the repository"), "{e}");
        assert!(reach_of(&files(&[("Cargo.toml", "not = [toml")])).is_err());
    }

    #[test]
    fn composed_names_reach_every_child() {
        let mut reach = Reach::default();
        scan_text(
            "cd sdk/examples/$(EXAMPLE) && build engine/examples/{name}",
            &mut reach,
        );
        assert!(reach.composed.contains("sdk/examples"));
        assert!(reach.composed.contains("engine/examples"));
        scan_text("mysdk/x my-engine/y", &mut reach);
        assert!(!reach
            .mentions
            .iter()
            .any(|m| m.ends_with("/x") || m.ends_with("/y")));
    }

    fn no_sources(_: &[String]) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }

    #[test]
    fn a_crate_that_reads_another_crates_file_reaches_it() {
        let reach = Reach {
            mentions: ["crates/psx-cook".to_string()].into(),
            ..Reach::default()
        };
        let mut sources = |dirs: &[String]| -> Result<Vec<(String, String)>, String> {
            Ok(dirs
                .iter()
                .filter(|d| *d == "crates/psx-cook")
                .map(|_| {
                    (
                        "crates/psx-cook/src/lib.rs".to_string(),
                        "const T: &str = include_str!(\"../../../engine/examples/editor-playtest/src/x.rs\");"
                            .to_string(),
                    )
                })
                .collect())
        };
        let got = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut sources)
            .unwrap()
            .reached;
        assert!(got.contains("engine/examples/editor-playtest"), "{got:?}");
        assert!(!got.contains("engine/examples/hardware-tests"));
    }

    fn sdk() -> BTreeMap<String, String> {
        [
            ("", "[workspace]\nmembers = [\"tools/psoxide-link\"]\n[workspace.dependencies]\nserde = \"1\"\n"),
            ("sdk", "[workspace]\nmembers = [\"crates/psx-rt\"]\n[workspace.dependencies]\npsx-hw = { path = \"../crates/psx-hw\" }\n"),
            ("sdk/crates/psx-rt", "[package]\nname = \"psx-rt\"\nedition.workspace = true\n[dependencies]\npsx-hw = { workspace = true }\n"),
            ("crates/psx-hw", "[package]\nname = \"psx-hw\"\n[dependencies]\nserde = { workspace = true }\n"),
            ("crates/psx-cook", "[package]\nname = \"psx-cook\"\n"),
            ("tools/psoxide-link", "[package]\nname = \"psoxide-link\"\n[dependencies]\npsx-iso = { path = \"../../crates/psx-iso\" }\n"),
            ("crates/psx-iso", "[package]\nname = \"psx-iso\"\n"),
            ("engine/examples/hardware-tests", "[package]\nname = \"hardware-tests\"\n[dependencies]\npsx-rt = { path = \"../../../sdk/crates/psx-rt\" }\n"),
            ("engine/examples/editor-playtest", "[package]\nname = \"editor-playtest\"\n"),
        ]
        .into_iter()
        .map(|(d, t)| (d.to_string(), t.to_string()))
        .collect()
    }

    #[test]
    fn the_closure_follows_paths_workspaces_and_tools() {
        let reach = Reach {
            mentions: ["sdk/crates/psx-rt".to_string()].into(),
            ..Reach::default()
        };
        let got = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources)
            .unwrap()
            .reached;
        let want: BTreeSet<String> = [
            "sdk/crates/psx-rt",
            "crates/psx-hw",
            "tools/psoxide-link",
            "crates/psx-iso",
        ]
        .map(str::to_string)
        .into();
        assert_eq!(got, want);
        // A tool script the game names brings what it names.
        let reach = Reach {
            tools: ["tools/engine-examples.mk".to_string()].into(),
            ..Reach::default()
        };
        let tools = [(
            "tools/engine-examples.mk".to_string(),
            "\tcd engine/examples/hardware-tests && cargo build\n".to_string(),
        )]
        .into();
        let got = reached_crates(&sdk(), &tools, &reach, &mut no_sources)
            .unwrap()
            .reached;
        assert!(got.contains("engine/examples/hardware-tests"));
        assert!(got.contains("sdk/crates/psx-rt"), "its dependencies too");
        assert!(!got.contains("engine/examples/editor-playtest"));
    }

    /// The SDK of `sdk()` plus two crates that read assets and one that reads none.
    fn asset_reader(
        text: &'static str,
    ) -> impl FnMut(&[String]) -> Result<Vec<(String, String)>, String> {
        move |dirs: &[String]| {
            Ok(dirs
                .iter()
                .filter(|d| *d == "engine/examples/hardware-tests")
                .map(|_| {
                    (
                        "engine/examples/hardware-tests/src/main.rs".to_string(),
                        text.to_string(),
                    )
                })
                .collect())
        }
    }

    #[test]
    fn an_asset_a_reached_crate_includes_counts_for_the_game() {
        let reach = Reach {
            mentions: ["engine/examples/hardware-tests".to_string()].into(),
            ..Reach::default()
        };
        let mut sources = asset_reader(
            "static B: &[u8] = include_bytes!(\"../../../../assets/textures/floor.psxt\");",
        );
        let reached = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut sources).unwrap();
        assert!(reached.keeps("assets/textures/floor.psxt"));
        assert!(!reached.keeps("assets/textures/brick-wall.psxt"));
        assert!(!reached.keeps("assets/audio/freesfx/psau/jump.psau"));
        // The compose form with the crate's manifest directory reads the same file.
        let mut sources = asset_reader(
            "include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../../assets/audio/x.psau\"));",
        );
        let reached = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut sources).unwrap();
        assert!(reached.keeps("assets/audio/x.psau"));
    }

    #[test]
    fn an_asset_no_reached_crate_reads_counts_for_no_one() {
        // The reading crate is out of reach: its asset does not count.
        let reach = Reach {
            mentions: ["crates/psx-cook".to_string()].into(),
            ..Reach::default()
        };
        let mut sources = asset_reader(
            "static B: &[u8] = include_bytes!(\"../../../../assets/textures/floor.psxt\");",
        );
        let reached = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut sources).unwrap();
        assert!(!reached.keeps("assets/textures/floor.psxt"));
        assert!(
            reached.keeps("sdk/psoxide.ld"),
            "files outside assets are unchanged"
        );
    }

    #[test]
    fn a_game_that_names_an_asset_directory_counts_it() {
        let reach = reach_of(&files(&[(
            "components.lock.json",
            "\"paths\": [\n \"assets/audio\",\n \"assets/branding/x.png\"\n]",
        )]))
        .unwrap();
        let reached = reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources).unwrap();
        assert!(reached.keeps("assets/audio/freesfx/psau/jump.psau"));
        assert!(!reached.keeps("assets/textures/floor.psxt"));
        // A file of the game naming a directory does too; `my-assets/` is another name.
        let reach = reach_of(&files(&[(
            "Makefile",
            "T := $(SDK)/assets/textures\nX := my-assets/audio\n",
        )]))
        .unwrap();
        assert!(reach.assets.contains("assets/textures"));
        assert!(!reach.assets.contains("assets/audio"));
    }

    #[test]
    fn a_file_counts_unless_every_crate_around_it_is_out_of_reach() {
        let reached = Reached {
            crates: [
                "engine/examples/hardware-tests",
                "crates/a",
                "crates/a/fixtures/b",
            ]
            .map(str::to_string)
            .into(),
            reached: ["crates/a".to_string()].into(),
            assets: BTreeSet::new(),
        };
        assert!(reached.keeps("sdk/psoxide.ld"));
        assert!(reached.keeps("engine/Cargo.lock"));
        assert!(!reached.keeps("engine/examples/hardware-tests/src/main.rs"));
        assert!(reached.keeps("crates/a/src/lib.rs"));
        // A crate nested in a reached one still counts: the outer one may read its files.
        assert!(reached.keeps("crates/a/fixtures/b/x.rs"));
        assert!(reached.keeps("engine/examples/hardware-tests-not/x"));
    }

    #[test]
    fn an_unknown_git_package_or_a_missing_workspace_entry_is_an_error() {
        let reach = Reach {
            git_packages: ["psx-nothing".to_string()].into(),
            ..Reach::default()
        };
        assert!(reached_crates(&sdk(), &BTreeMap::new(), &reach, &mut no_sources).is_err());
        let mut broken = sdk();
        broken.insert(
            "sdk/crates/psx-rt".into(),
            "[package]\nname = \"psx-rt\"\n[dependencies]\npsx-gone = { workspace = true }\n"
                .into(),
        );
        let reach = Reach {
            mentions: ["sdk/crates/psx-rt".to_string()].into(),
            ..Reach::default()
        };
        assert!(reached_crates(&broken, &BTreeMap::new(), &reach, &mut no_sources).is_err());
    }

    /// Every manifest of this SDK checkout under the guest paths (directory -> text), and the
    /// root's under "".
    fn checkout_manifests(root: &Path) -> BTreeMap<String, String> {
        fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if name == "target" || name == "build" || name == ".git" {
                    continue;
                }
                if path.is_dir() {
                    visit(root, &path, out);
                } else if name == "Cargo.toml" {
                    let rel = path.parent().unwrap().strip_prefix(root).unwrap();
                    out.insert(
                        rel.display().to_string(),
                        std::fs::read_to_string(&path).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        for g in GUEST_PATHS {
            visit(root, &root.join(g), &mut out);
        }
        out.insert(
            String::new(),
            std::fs::read_to_string(root.join("Cargo.toml")).unwrap(),
        );
        out
    }

    /// What the scope assumes of this SDK, checked against it: no guest tool crate names an
    /// example (every game runs those tools, so an example they build would be out of every
    /// scope), and the closure over this checkout resolves, finding the relative reads it has.
    #[test]
    fn the_scope_resolves_over_this_sdk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifests = checkout_manifests(&root);
        let mut read = |dirs: &[String]| -> Result<Vec<(String, String)>, String> {
            let mut out = Vec::new();
            let mut stack: Vec<std::path::PathBuf> = dirs.iter().map(|d| root.join(d)).collect();
            while let Some(at) = stack.pop() {
                for entry in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                    let path = entry.path();
                    let name = entry.file_name();
                    if path.is_dir() {
                        if name != "target" && name != "build" {
                            stack.push(path);
                        }
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        let rel = path.strip_prefix(&root).unwrap().display().to_string();
                        out.push((rel, std::fs::read_to_string(&path).unwrap_or_default()));
                    }
                }
            }
            Ok(out)
        };
        // psx-rt's present model reads psx-cdstream's sources, so a game linking psx-rt reaches it.
        let reach = Reach {
            mentions: ["sdk/crates/psx-rt".to_string()].into(),
            ..Reach::default()
        };
        let got = reached_crates(&manifests, &BTreeMap::new(), &reach, &mut read).unwrap();
        assert!(got.reached.contains("sdk/crates/psx-cdstream"));
        assert!(!got.reached.contains("engine/examples/hardware-tests"));
        let mut problems = Vec::new();
        for tool in got.reached.iter().filter(|d| d.starts_with("tools/")) {
            for (path, text) in read(std::slice::from_ref(tool)).unwrap() {
                for example in got.crates.iter().filter(|c| c.contains("/examples/")) {
                    if text.contains(example.as_str()) {
                        problems.push(format!("{path} names the example {example}"));
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
