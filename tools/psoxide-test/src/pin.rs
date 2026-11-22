//! The SDK pin rule: a game may pin any SDK commit whose GUEST TREE equals SDK main's.
//!
//! The guest tree of an SDK commit is everything that can change the bytes of a game's disc:
//! the guest crates and their build files (`sdk/`, `engine/`), the host crates that cook assets
//! or link and patch the executable (`crates/`), the build tools that turn a guest link into a
//! disc (`tools/` entries in [`GUEST_PATHS`]), and the toolchain file. Documentation (any `docs`
//! directory, any `*.md`) is left out. The root `Cargo.toml` counts with its workspace member
//! list removed (adding a host tool to the workspace changes no other crate), and the root
//! `Cargo.lock` counts only for the packages the guest-path workspace members depend on (a host
//! tool's new dependency does not move it). The result is one SHA-256, computed from git objects
//! of a bare, blob-less mirror of the SDK repository, never from a checkout.
//!
//! A host-tool-only commit (this harness, psoxide-dev, the docs) leaves the guest tree equal, so
//! it never forces a repin.
//!
//! The rule compares one game's view of that tree (scope.rs): the same digest without the files
//! of SDK crates the game cannot reach, so a change to a disc no game links (an SDK example)
//! moves no game that does not build it. When the game's reach cannot be read, the whole guest
//! tree is compared. A lock records the whole guest tree of its pin, which does not depend on
//! the game.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::game::Game;
use crate::scope::{self, Scope};
use crate::util::{self, Key};

/// Bumped whenever the guest-tree definition changes, so cached trees of the old one are not reused.
pub const DEFINITION: &str = "guest-tree-2";

/// The same for a game's scoped view of it (scope.rs).
pub const SCOPED_DEFINITION: &str = "guest-tree-scoped-2";

/// The source of the code that computes a tree. A cached tree is kept under a hash of it as
/// well as the definition names above, so a change to the logic never reads a tree the old
/// logic computed, bumped or not.
const LOGIC: [&str; 3] = [
    include_str!("pin.rs"),
    include_str!("scope.rs"),
    include_str!("util.rs"),
];

/// The hash of [`LOGIC`], short.
fn logic_digest() -> String {
    let mut key = Key::new("guest-tree-logic");
    for (i, source) in LOGIC.iter().enumerate() {
        key.part(&i.to_string(), source);
    }
    key.finish()[..16].to_string()
}

/// The SDK paths whose content can change a game's bytes (see the module documentation).
pub const GUEST_PATHS: &[&str] = &[
    "sdk",
    "engine",
    "crates",
    "assets/audio",
    "assets/textures",
    "tools/mkisopsx",
    "tools/psoxide-link",
    "tools/psoxide-pgo",
    "tools/psoxide-hazard",
    "tools/psoxide-size",
    "tools/guest_symbol_gate.sh",
    "tools/build_guest_staged.sh",
    "tools/sdk-examples.mk",
    "tools/engine-examples.mk",
    "rust-toolchain.toml",
];

/// Where a game pins its SDK, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The pinned SDK commit (full or abbreviated hex).
    pub revision: String,
    /// `owner/name` on GitHub.
    pub repository: String,
    /// The guest tree the lock records, when it records one.
    pub recorded: Option<String>,
    /// The file the pin was read from.
    pub file: String,
}

/// `owner/name` from `owner/name`, `https://github.com/owner/name(.git)`.
fn slug(repository: &str) -> String {
    repository
        .trim_start_matches("https://github.com/")
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_string()
}

/// Read the pin: `components.lock.json` (`components.sdk`), `sdk.lock.json` (`revision`), or the
/// game file's `[sdk]` (a revision written in a script, found after a marker text).
pub fn read(repo: &Path, game: Option<&Game>) -> Option<Pin> {
    let json = |file: &str| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(repo.join(file)).ok()?).ok()
    };
    let text = |v: &serde_json::Value, p: &str| v.pointer(p)?.as_str().map(str::to_string);
    if let Some(v) = json("components.lock.json") {
        if let Some(revision) = text(&v, "/components/sdk/revision") {
            return Some(Pin {
                revision,
                repository: slug(
                    &text(&v, "/components/sdk/repository").unwrap_or("EBonura/PSoXide".into()),
                ),
                recorded: text(&v, "/components/sdk/guest_tree"),
                file: "components.lock.json".into(),
            });
        }
    }
    if let Some(v) = json("sdk.lock.json") {
        if let Some(revision) = text(&v, "/revision") {
            return Some(Pin {
                revision,
                repository: slug(&text(&v, "/repository").unwrap_or("EBonura/PSoXide".into())),
                recorded: text(&v, "/guest_tree"),
                file: "sdk.lock.json".into(),
            });
        }
    }
    let sdk = game?.sdk.as_ref()?;
    let (file, after) = (sdk.file.as_ref()?, sdk.after.as_ref()?);
    let body = std::fs::read_to_string(repo.join(file)).ok()?;
    let at = body.find(after.as_str())? + after.len();
    let revision: String = body[at..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (revision.len() >= 7).then(|| Pin {
        revision,
        repository: slug(&sdk.repository),
        recorded: sdk.guest_tree.clone(),
        file: file.clone(),
    })
}

/// A bare, blob-less mirror of the SDK repository under the work tree.
pub struct Mirror {
    dir: PathBuf,
    /// Seconds since the last successful fetch, when this run could not fetch (offline).
    pub stale: Option<u64>,
}

impl Mirror {
    /// Clone it once, then fetch every branch (trees and commits only; blobs on demand).
    pub fn open(work_root: &Path, repository: &str) -> Result<Mirror, String> {
        let dir = work_root
            .join("sdk-mirror")
            .join(slug(repository).replace('/', "-"));
        let url = format!("https://github.com/{}.git", slug(repository));
        if !dir.join("HEAD").is_file() {
            std::fs::create_dir_all(dir.parent().unwrap_or(work_root))
                .map_err(|e| e.to_string())?;
            let out = std::process::Command::new("git")
                .args(["clone", "-q", "--bare", "--filter=blob:none", &url])
                .arg(&dir)
                .output()
                .map_err(|e| format!("cannot run git: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "cannot mirror {url}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        let mut mirror = Mirror { dir, stale: None };
        let stamp = mirror.dir.join("psoxide-test-last-fetch");
        match mirror.git(&["fetch", "-q", "origin", "+refs/heads/*:refs/heads/*"]) {
            Ok(_) => {
                let _ = std::fs::write(&stamp, "");
            }
            // Offline: the main fetched last time stands, with its age; never fetched is an error.
            Err(e) => match util::mtime_seconds(&stamp) {
                Some(t) => mirror.stale = Some((util::now_seconds() - t).max(0.0) as u64),
                None => {
                    return Err(format!(
                        "cannot fetch SDK main and none was ever fetched: {e}"
                    ))
                }
            },
        }
        Ok(mirror)
    }

    fn git(&self, args: &[&str]) -> Result<String, String> {
        util::git(&self.dir, args)
    }

    /// The full commit of `rev`, fetching it when no branch holds it.
    pub fn commit(&self, rev: &str) -> Result<String, String> {
        let spec = format!("{rev}^{{commit}}");
        if let Ok(full) = self.git(&["rev-parse", "--verify", "-q", &spec]) {
            return Ok(full);
        }
        self.git(&["fetch", "-q", "--filter=blob:none", "origin", rev])?;
        self.git(&["rev-parse", "--verify", "-q", &spec])
    }

    /// The guest tree of `commit` (see the module documentation).
    pub fn guest_tree(&self, commit: &str) -> Result<String, String> {
        self.cached(&format!("{DEFINITION}:{}:{commit}", logic_digest()), || {
            self.compute_guest_tree(commit, None)
        })
    }

    /// The guest tree of `commit` as a game with `reach` sees it, and how many of the commit's
    /// crates that game reaches (`12/120`).
    pub fn scoped_guest_tree(
        &self,
        commit: &str,
        reach: &scope::Reach,
    ) -> Result<(String, String), String> {
        let slot = format!(
            "{SCOPED_DEFINITION}:{}:{commit}:{}",
            logic_digest(),
            reach.digest()
        );
        let value = self.cached(&slot, || self.compute_guest_tree(commit, Some(reach)))?;
        let (tree, counts) = value.split_once(' ').unwrap_or((&value, "?"));
        Ok((tree.to_string(), counts.to_string()))
    }

    /// A commit's tree never changes: kept by definition version, so an offline run and a
    /// repeated run read it back instead of recomputing.
    fn cached(
        &self,
        slot: &str,
        compute: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        let cache = self.dir.join("psoxide-test-guest-trees.json");
        let mut known: BTreeMap<String, String> = std::fs::read_to_string(&cache)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if let Some(tree) = known.get(slot) {
            return Ok(tree.clone());
        }
        let tree = compute()?;
        known.insert(slot.to_string(), tree.clone());
        if let Ok(json) = serde_json::to_string_pretty(&known) {
            let _ = std::fs::write(&cache, json);
        }
        Ok(tree)
    }

    fn compute_guest_tree(
        &self,
        commit: &str,
        reach: Option<&scope::Reach>,
    ) -> Result<String, String> {
        let mut args = vec!["ls-tree", "-r", "--full-tree", commit, "--"];
        args.extend(GUEST_PATHS);
        let listing = self.git(&args)?;
        let manifest = self.git(&["show", &format!("{commit}:Cargo.toml")]).ok();
        let lock = self.git(&["show", &format!("{commit}:Cargo.lock")]).ok();
        let member_names = |member: &str| {
            self.git(&["show", &format!("{commit}:{member}/Cargo.toml")])
                .ok()
                .and_then(|t| toml::from_str::<toml::Table>(&t).ok())
                .and_then(|t| t.get("package")?.get("name")?.as_str().map(str::to_string))
        };
        let Some(reach) = reach else {
            return guest_tree_of(&listing, manifest.as_deref(), lock.as_deref(), member_names);
        };
        let reached = self.reached_in(commit, &listing, manifest.as_deref(), reach)?;
        let tree = tree_of(
            "sdk-guest-tree-scoped",
            &listing,
            manifest.as_deref(),
            lock.as_deref(),
            member_names,
            |path| reached.keeps(path),
        )?;
        Ok(format!(
            "{tree} {}/{}",
            reached.reached.len(),
            reached.crates.len()
        ))
    }

    /// The crates of `commit` a game with `reach` reaches (`listing` is its `ls-tree` of the
    /// guest paths, `manifest` its root manifest).
    fn reached_in(
        &self,
        commit: &str,
        listing: &str,
        manifest: Option<&str>,
        reach: &scope::Reach,
    ) -> Result<scope::Reached, String> {
        // Every manifest of the guest paths (and the root's), and the tool scripts the game names.
        let mut wanted: Vec<(String, String)> = Vec::new();
        for (oid, path) in listing_blobs(listing) {
            if path.ends_with("/Cargo.toml") || scope::is_guest_tool_file(path) {
                wanted.push((oid.to_string(), path.to_string()));
            }
        }
        let oids: Vec<&str> = wanted.iter().map(|(oid, _)| oid.as_str()).collect();
        let texts = self.blobs(&oids)?;
        let mut manifests = BTreeMap::new();
        let mut tools = BTreeMap::new();
        for (oid, path) in &wanted {
            let text = texts
                .get(oid)
                .ok_or_else(|| format!("{commit}:{path} cannot be read"))?
                .clone();
            if scope::is_guest_tool_file(path) {
                tools.insert(path.clone(), text);
            } else if let Some(dir) = path.strip_suffix("/Cargo.toml") {
                manifests.insert(dir.to_string(), text);
            }
        }
        if let Some(root) = manifest {
            manifests.insert(String::new(), root.to_string());
        }
        let rust: Vec<(&str, &str)> = listing_blobs(listing)
            .filter(|(_, path)| path.ends_with(".rs"))
            .collect();
        let mut sources = |dirs: &[String]| -> Result<Vec<(String, String)>, String> {
            let wanted: Vec<(&str, &str)> = rust
                .iter()
                .copied()
                .filter(|(_, path)| {
                    dirs.iter().any(|d| {
                        path.strip_prefix(d.as_str())
                            .is_some_and(|r| r.starts_with('/'))
                    })
                })
                .collect();
            let oids: Vec<&str> = wanted.iter().map(|(oid, _)| *oid).collect();
            let texts = self.blobs(&oids)?;
            wanted
                .iter()
                .map(|(oid, path)| {
                    let text = texts
                        .get(*oid)
                        .ok_or_else(|| format!("{commit}:{path} cannot be read"))?;
                    Ok((path.to_string(), text.clone()))
                })
                .collect()
        };
        scope::reached_crates(&manifests, &tools, reach, &mut sources)
    }

    /// [`Mirror::reached_in`] for a commit, uncached (`psoxide-test pin --crates`).
    pub fn reached(&self, commit: &str, reach: &scope::Reach) -> Result<scope::Reached, String> {
        let mut args = vec!["ls-tree", "-r", "--full-tree", commit, "--"];
        args.extend(GUEST_PATHS);
        let listing = self.git(&args)?;
        let manifest = self.git(&["show", &format!("{commit}:Cargo.toml")]).ok();
        self.reached_in(commit, &listing, manifest.as_deref(), reach)
    }

    /// The texts of `oids`, fetched in one batch when the blob-less mirror lacks them.
    fn blobs(&self, oids: &[&str]) -> Result<BTreeMap<String, String>, String> {
        let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
        // What is missing, without a fetch per object.
        let check = util::git_stdin(
            &self.dir,
            &["cat-file", "--batch-check"],
            input.as_bytes(),
            &[("GIT_NO_LAZY_FETCH", "1")],
        )?;
        let missing: Vec<String> = String::from_utf8_lossy(&check)
            .lines()
            .filter_map(|l| l.strip_suffix(" missing").map(str::to_string))
            .collect();
        if !missing.is_empty() {
            // The fetch git makes for one lazy object, for all of them at once (a blob cannot be
            // fetched as a revision on the command line). If it fails, `--batch` below still
            // fetches each object itself, slowly.
            let wants: String = missing.iter().map(|oid| format!("{oid}\n")).collect();
            let _ = util::git_stdin(
                &self.dir,
                &[
                    "-c",
                    "fetch.negotiationAlgorithm=noop",
                    "fetch",
                    "-q",
                    "origin",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--recurse-submodules=no",
                    "--filter=blob:none",
                    "--stdin",
                ],
                wants.as_bytes(),
                &[],
            );
        }
        let out = util::git_stdin(&self.dir, &["cat-file", "--batch"], input.as_bytes(), &[])?;
        parse_batch(&out)
    }
}

/// `(oid, path)` of every blob in an `ls-tree -r` listing.
fn listing_blobs(listing: &str) -> impl Iterator<Item = (&str, &str)> {
    listing.lines().filter_map(|line| {
        let (meta, path) = line.split_once('\t')?;
        let mut fields = meta.split(' ');
        let (_, kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
        (kind == "blob").then_some((oid, path))
    })
}

/// The objects of `git cat-file --batch` output, by id (text, lossily).
fn parse_batch(out: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut objects = BTreeMap::new();
    let mut at = 0;
    while at < out.len() {
        let end = out[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| at + i)
            .ok_or("cat-file: a header without its line end")?;
        let header = String::from_utf8_lossy(&out[at..end]).to_string();
        let fields: Vec<&str> = header.split(' ').collect();
        let [oid, _, size] = fields.as_slice() else {
            return Err(format!("cat-file: {header}"));
        };
        let size: usize = size.parse().map_err(|_| format!("cat-file: {header}"))?;
        let body = out
            .get(end + 1..end + 1 + size)
            .ok_or_else(|| format!("cat-file: {oid} is cut short"))?;
        objects.insert(oid.to_string(), String::from_utf8_lossy(body).into_owned());
        at = end + 1 + size + 1;
    }
    Ok(objects)
}

/// Whether a path is documentation.
pub fn is_doc(path: &str) -> bool {
    path.ends_with(".md") || path.split('/').any(|c| c == "docs")
}

/// The guest tree from an `ls-tree -r` listing, the root manifest and lock, and a way to read a
/// workspace member's package name.
pub fn guest_tree_of(
    listing: &str,
    manifest: Option<&str>,
    lock: Option<&str>,
    member_name: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    tree_of(
        "sdk-guest-tree",
        listing,
        manifest,
        lock,
        member_name,
        |_| true,
    )
}

/// [`guest_tree_of`] over only the listed files `keep` accepts, under the key `label`.
fn tree_of(
    label: &str,
    listing: &str,
    manifest: Option<&str>,
    lock: Option<&str>,
    member_name: impl Fn(&str) -> Option<String>,
    keep: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let mut key = Key::new(label);
    for line in listing.lines() {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        if !is_doc(path) && keep(path) {
            key.part(path, meta);
        }
    }
    let mut roots = BTreeSet::new();
    if let Some(manifest) = manifest {
        let mut table: toml::Table = toml::from_str(manifest).map_err(|e| e.to_string())?;
        if let Some(ws) = table
            .get_mut("workspace")
            .and_then(toml::Value::as_table_mut)
        {
            let members: Vec<String> = ws
                .get("members")
                .and_then(toml::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for m in members {
                let guest = GUEST_PATHS
                    .iter()
                    .any(|g| m == *g || m.starts_with(&format!("{g}/")));
                if guest {
                    if let Some(name) = member_name(&m) {
                        roots.insert(name);
                    }
                }
            }
            ws.remove("members");
            ws.remove("exclude");
        }
        key.part(
            "Cargo.toml",
            &toml::to_string(&table).map_err(|e| e.to_string())?,
        );
    }
    if let Some(lock) = lock {
        for package in lock_closure(lock, &roots)? {
            key.part("Cargo.lock", &package);
        }
    }
    Ok(key.finish())
}

/// The `[[package]]` entries of a Cargo.lock reachable from `roots`, one line each, sorted.
fn lock_closure(lock: &str, roots: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let table: toml::Table = toml::from_str(lock).map_err(|e| format!("Cargo.lock: {e}"))?;
    let packages = table
        .get("package")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let field =
        |p: &toml::Value, k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // name -> (version -> (line, dependency names))
    let mut by_name: BTreeMap<String, Vec<(String, String, Vec<String>)>> = BTreeMap::new();
    for p in &packages {
        let deps: Vec<String> = p
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let line = format!(
            "{} {} {} {}",
            field(p, "name"),
            field(p, "version"),
            field(p, "source"),
            field(p, "checksum")
        );
        by_name
            .entry(field(p, "name"))
            .or_default()
            .push((field(p, "version"), line, deps));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<(String, Option<String>)> =
        roots.iter().map(|r| (r.clone(), None)).collect();
    while let Some((name, version)) = stack.pop() {
        for (v, line, deps) in by_name.get(&name).into_iter().flatten() {
            if version.as_ref().is_some_and(|want| want != v) || !seen.insert(line.clone()) {
                continue;
            }
            for d in deps {
                // "name", "name version" or "name version (source)".
                let mut parts = d.split(' ');
                let n = parts.next().unwrap_or_default().to_string();
                stack.push((n, parts.next().map(str::to_string)));
            }
        }
    }
    Ok(seen.into_iter().collect())
}

/// The verdict of the pin rule for one run.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub pinned: String,
    pub main: String,
    /// The trees compared: the game's view of each commit (or the whole tree, see `scope`).
    pub pinned_tree: String,
    pub main_tree: String,
    /// The whole guest trees, which a lock records.
    pub pinned_whole: String,
    pub main_whole: String,
    /// What was compared, in words.
    pub scope: String,
    pub recorded: Option<String>,
    pub file: String,
    /// Offline: seconds since SDK main was last fetched (the main judged is that one).
    pub main_age: Option<u64>,
}

impl Verdict {
    /// The rule holds and the lock (if it records a tree) is right.
    pub fn holds(&self) -> bool {
        self.pinned_tree == self.main_tree
            && self
                .recorded
                .as_ref()
                .is_none_or(|r| *r == self.pinned_whole)
    }

    /// One line for the result header.
    pub fn header(&self) -> String {
        let line = if self.pinned_tree == self.main_tree {
            format!("guest tree equals main {}", util::short(&self.main))
        } else {
            format!(
                "guest tree differs from main {}: repin",
                util::short(&self.main)
            )
        };
        let line = if self.pinned_tree == self.main_tree && self.pinned_whole != self.main_whole {
            format!("{line} for this game (main changed only SDK crates it does not reach)")
        } else {
            line
        };
        match self.main_age {
            Some(age) => format!("{line} (offline: main as fetched {} ago)", age_text(age)),
            None => line,
        }
    }
}

/// `3 h 05 min`, `12 min`, `40 s`.
pub fn age_text(seconds: u64) -> String {
    match seconds {
        s if s >= 86_400 => format!("{} d {} h", s / 86_400, s % 86_400 / 3600),
        s if s >= 3600 => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// Apply the rule in `mirror` (an open [`Mirror`] of the pin's repository): the pinned commit's
/// guest tree against SDK main's (or against `main`, a candidate revision), as a game of `scope`
/// sees them.
pub fn judge(
    mirror: &Mirror,
    pin: &Pin,
    scope: &Scope,
    main: Option<&str>,
) -> Result<Verdict, String> {
    let pinned = mirror.commit(&pin.revision)?;
    let main = mirror.commit(main.unwrap_or("refs/heads/main"))?;
    let trees = Trees::of(mirror, &pinned, &main, scope)?;
    Ok(Verdict {
        pinned_tree: trees.pinned,
        main_tree: trees.main,
        pinned_whole: trees.pinned_whole,
        main_whole: trees.main_whole,
        scope: trees.scope,
        pinned,
        main,
        recorded: pin.recorded.clone(),
        file: pin.file.clone(),
        main_age: mirror.stale,
    })
}

/// How long after the first SDK guest change a game lacks a `[sdk.defer]` may end.
pub const DEFER_WINDOW_SECONDS: u64 = 48 * 3600;

/// How many first-parent commits of SDK main [`first_missed_move`] reads before giving up.
pub const DEFER_WALK: usize = 200;

/// Where a stale pin left SDK main: the oldest commit on main's first-parent line that moved the
/// game's guest tree after the last commit whose tree the pin still equals.
#[derive(Debug, Clone)]
pub struct Move {
    pub commit: String,
    /// Its first parent: the SDK the game is current on.
    pub parent: String,
    /// Its commit time, seconds since the epoch.
    pub time: u64,
    /// How many commits from it to main moved the game's guest tree (it included).
    pub missed: usize,
}

/// Walk `main`'s first-parent history (at most `limit` commits) back to the newest commit whose
/// guest tree, as a game of `scope` sees it, equals `pinned`'s, and return the move right after
/// it ([`Move`]). An error when the pin is not found within `limit` commits.
///
/// One SDK landing is often several commits (a lane's branch fast-forwarded onto main), and
/// git does not record where one landing ends, so "behind" is measured from the first change
/// the game lacks: the deferral window starts at that commit's time, which is no later than the
/// landing that brought it.
pub fn first_missed_move(
    mirror: &Mirror,
    pinned: &str,
    main: &str,
    scope: &Scope,
    limit: usize,
) -> Result<Move, String> {
    let log = mirror.git(&[
        "log",
        "--first-parent",
        &format!("--max-count={limit}"),
        "--format=%H %ct %P",
        main,
    ])?;
    let mut missed = 0;
    for line in log.lines() {
        let mut fields = line.split(' ');
        let (Some(commit), Some(time)) = (fields.next(), fields.next()) else {
            return Err(format!("git log: {line}"));
        };
        let Some(parent) = fields.next().filter(|p| !p.is_empty()) else {
            return Err(format!(
                "reached the first commit {} without finding the pin's guest tree",
                util::short(commit)
            ));
        };
        // Every input of either tree lies under these paths: a commit outside them moves none.
        let mut args = vec!["diff-tree", "-r", "--name-only", parent, commit, "--"];
        args.extend(GUEST_PATHS);
        args.extend(["Cargo.toml", "Cargo.lock"]);
        if mirror.git(&args)?.trim().is_empty() {
            continue;
        }
        let step = Trees::of(mirror, parent, commit, scope)?;
        if step.pinned == step.main {
            continue;
        }
        missed += 1;
        let before = Trees::of(mirror, pinned, parent, scope)?;
        if before.pinned == before.main {
            return Ok(Move {
                commit: commit.to_string(),
                parent: parent.to_string(),
                time: time.parse().map_err(|_| format!("git log: {line}"))?,
                missed,
            });
        }
    }
    Err(format!(
        "its guest tree is not that of any of the last {limit} commits of main"
    ))
}

/// [`first_missed_move`] for a verdict's pin and main, in words a refusal can carry.
pub fn missed_move(mirror: &Mirror, verdict: &Verdict, scope: &Scope) -> Result<Move, String> {
    first_missed_move(mirror, &verdict.pinned, &verdict.main, scope, DEFER_WALK)
        .map_err(|e| format!("where the pin left main cannot be found: {e}"))
}

/// A pin rule failure a `[sdk.defer]` turns into a deferred pass (landing runs only).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Deferred {
    /// RFC 3339 UTC, as the game file writes it.
    pub until: String,
    pub reason: String,
    /// The first SDK main commit whose guest change the pin lacks, its commit time (RFC 3339),
    /// and how many commits up to main moved the game's guest tree.
    pub move_commit: String,
    pub move_time: String,
    pub missed: usize,
}

/// Whether `defer` holds for `verdict` (whose pin differs from main): `Ok` when it does, else why
/// not, in words that finish "the [sdk.defer] did not apply: ". It holds only when every one of:
///
/// - its reason is not empty and `until` is an RFC 3339 UTC time;
/// - the run is the landing profile (an install run, and so any install, needs a current pin);
/// - `now` is before `until`;
/// - the pin's guest tree is that of a recent commit of main ([`first_missed_move`]), and
///   `until` is at most [`DEFER_WINDOW_SECONDS`] after the first commit since then that moved
///   the game's guest tree. A game file cannot write a far future date, and a game can never
///   trail a guest change of main by more than the window, however many landings follow it: a
///   game two moves behind whose first missed move is older than the window is refused.
///
/// Any error reading the history is a refusal.
pub fn deferral(
    mirror: &Mirror,
    verdict: &Verdict,
    scope: &Scope,
    defer: &crate::game::SdkDefer,
    profile: crate::stage::Profile,
    now: u64,
) -> Result<Deferred, String> {
    if defer.reason.trim().is_empty() {
        return Err("its `reason` is empty".into());
    }
    let until = util::parse_rfc3339_utc(&defer.until).ok_or_else(|| {
        format!(
            "`until = \"{}\"` is not an RFC 3339 UTC time (YYYY-MM-DDTHH:MM:SSZ)",
            defer.until
        )
    })?;
    if profile != crate::stage::Profile::Landing {
        return Err(format!(
            "a deferral holds for `run --tier full --profile landing` only; this is the {} \
             profile, which needs a current pin",
            profile.name()
        ));
    }
    if now >= until {
        return Err(format!(
            "it expired at {} ({} ago): repin to main",
            defer.until,
            age_text(now - until)
        ));
    }
    let moved = missed_move(mirror, verdict, scope)?;
    if until > moved.time + DEFER_WINDOW_SECONDS {
        return Err(format!(
            "`until` {} is more than {} h after {} ({}), the first guest change of main the pin \
             lacks ({} guest-moving commits behind): the latest it may be is {}",
            defer.until,
            DEFER_WINDOW_SECONDS / 3600,
            util::short(&moved.commit),
            util::rfc3339_utc(moved.time),
            moved.missed,
            util::rfc3339_utc(moved.time + DEFER_WINDOW_SECONDS)
        ));
    }
    Ok(Deferred {
        until: defer.until.clone(),
        reason: defer.reason.trim().to_string(),
        move_commit: moved.commit,
        move_time: util::rfc3339_utc(moved.time),
        missed: moved.missed,
    })
}

/// The trees one verdict compares.
#[derive(Debug)]
struct Trees {
    pinned: String,
    main: String,
    pinned_whole: String,
    main_whole: String,
    scope: String,
}

impl Trees {
    fn of(mirror: &Mirror, pinned: &str, main: &str, scope: &Scope) -> Result<Trees, String> {
        let pinned_whole = mirror.guest_tree(pinned)?;
        let main_whole = mirror.guest_tree(main)?;
        let whole = |why: &str| Trees {
            pinned: pinned_whole.clone(),
            main: main_whole.clone(),
            pinned_whole: pinned_whole.clone(),
            main_whole: main_whole.clone(),
            scope: format!("the whole guest tree ({why})"),
        };
        Ok(match scope {
            Scope::Game(reach) => {
                let scoped = mirror
                    .scoped_guest_tree(pinned, reach)
                    .and_then(|p| Ok((p, mirror.scoped_guest_tree(main, reach)?)));
                match scoped {
                    Ok(((p, _), (m, counts))) => Trees {
                        pinned: p,
                        main: m,
                        pinned_whole: pinned_whole.clone(),
                        main_whole: main_whole.clone(),
                        scope: format!("the SDK crates this game reaches ({counts} at main)"),
                    },
                    // Never narrower than what can be shown: the whole tree.
                    Err(e) => whole(&e),
                }
            }
            Scope::Whole(why) => whole(why),
        })
    }
}

/// `text` (a JSON lock) with `"guest_tree": "<tree>"` beside the first `"revision"` after
/// `section`, replacing one that is there; the rest of the file keeps its bytes and order.
pub fn record_guest_tree(text: &str, section: &str, tree: &str) -> Option<String> {
    // The end of a JSON string value whose key starts at `key_at`.
    let value_end = |key_at: usize| -> Option<usize> {
        let colon = key_at + text[key_at..].find(':')?;
        let open = colon + text[colon..].find('"')?;
        let close = open + 1 + text[open + 1..].find('"')?;
        Some(close + 1)
    };
    let start = text.find(section)?;
    let rev_at = start + text[start..].find("\"revision\"")?;
    let line_start = text[..rev_at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..rev_at];
    let rev_end = value_end(rev_at)?;
    let field = format!("\"guest_tree\": \"{tree}\"");
    let next = &text[rev_end..];
    let skipped = next.len() - next.trim_start_matches([',', '\n', ' ', '\t', '\r']).len();
    if next[skipped..].starts_with("\"guest_tree\"") {
        let at = rev_end + skipped;
        let end = value_end(at)?;
        return Some(format!("{}{field}{}", &text[..at], &text[end..]));
    }
    Some(format!(
        "{},\n{indent}{field}{}",
        &text[..rev_end],
        &text[rev_end..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The definition checks itself against this SDK: every path dependency of a guest-path
    /// workspace member, followed recursively, lies inside the guest paths. A guest tool that
    /// gains a dependency on a host-only crate fails here, not silently in a pin.
    #[test]
    fn every_path_dependency_of_a_guest_tool_is_in_the_guest_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest: toml::Table =
            toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
        let members: Vec<String> = manifest["workspace"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect();
        let inside = |p: &str| {
            GUEST_PATHS
                .iter()
                .any(|g| p == *g || p.starts_with(&format!("{g}/")))
        };
        let mut stack: Vec<String> = members.into_iter().filter(|m| inside(m)).collect();
        let mut seen = BTreeSet::new();
        while let Some(dir) = stack.pop() {
            if !seen.insert(dir.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(root.join(&dir).join("Cargo.toml")) else {
                continue;
            };
            let t: toml::Table = toml::from_str(&text).unwrap();
            for section in ["dependencies", "build-dependencies"] {
                for (_, dep) in t
                    .get(section)
                    .and_then(|d| d.as_table())
                    .into_iter()
                    .flatten()
                {
                    if let Some(rel) = dep.get("path").and_then(|p| p.as_str()) {
                        let joined = root.join(&dir).join(rel);
                        let canon = joined.canonicalize().unwrap();
                        let base = root.canonicalize().unwrap();
                        let path = canon.strip_prefix(&base).unwrap().display().to_string();
                        assert!(
                            inside(&path),
                            "{dir} depends on {path}, outside GUEST_PATHS"
                        );
                        stack.push(path);
                    }
                }
            }
        }
        assert!(seen.contains("tools/psoxide-pgo"));
    }

    /// A small SDK repository: the root workspace with one guest tool, the SDK workspace with
    /// psx-rt, and the hardware-tests example. Returns the directory and a commit function.
    fn sdk_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("Cargo.toml", "[workspace]\nmembers = [\"tools/psoxide-link\"]\n"),
            ("Cargo.lock", "version = 4\n[[package]]\nname = \"psoxide-link\"\nversion = \"0.1.0\"\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"nightly\"\n"),
            ("tools/psoxide-link/Cargo.toml", "[package]\nname = \"psoxide-link\"\n"),
            ("tools/psoxide-link/src/lib.rs", "pub fn link() {}\n"),
            ("sdk/Cargo.toml", "[workspace]\nmembers = [\"crates/psx-rt\"]\n"),
            ("sdk/psoxide.ld", "SECTIONS {}\n"),
            ("sdk/crates/psx-rt/Cargo.toml", "[package]\nname = \"psx-rt\"\n"),
            ("sdk/crates/psx-rt/src/lib.rs", "pub fn rt() {}\n"),
            ("engine/examples/hardware-tests/Cargo.toml", "[package]\nname = \"hardware-tests\"\n[dependencies]\npsx-rt = { path = \"../../../sdk/crates/psx-rt\" }\n"),
            ("engine/examples/hardware-tests/src/main.rs", "const SUITE_VERSION: &str = \"v2.25\";\n"),
        ];
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let git = |args: &[&str]| util::git(dir.path(), args).unwrap();
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "a",
        ]);
        dir
    }

    /// Edit `path` in `repo` and commit; the new commit.
    fn commit_edit(repo: &Path, path: &str, text: &str) -> String {
        std::fs::write(repo.join(path), text).unwrap();
        util::git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-am",
                "e",
            ],
        )
        .unwrap();
        util::git(repo, &["rev-parse", "HEAD"]).unwrap()
    }

    fn game(files: &[(&str, &str)]) -> Scope {
        let files: Vec<(String, String)> = files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        Scope::Game(Box::new(scope::reach_of(&files).unwrap()))
    }

    /// A game that links psx-rt: the files it builds from.
    const LINKS_RT: &[(&str, &str)] = &[(
        "game/Cargo.toml",
        "[package]\nname = \"g\"\n[dependencies]\npsx-rt = { path = \"../.psoxide/sdk/crates/psx-rt\" }\n",
    )];

    #[test]
    fn a_change_to_an_example_a_game_does_not_build_does_not_refuse_it() {
        let repo = sdk_repo();
        let mirror = Mirror {
            dir: repo.path().to_path_buf(),
            stale: None,
        };
        let base = util::git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let hwtest = commit_edit(
            repo.path(),
            "engine/examples/hardware-tests/src/main.rs",
            "const SUITE_VERSION: &str = \"v2.26\";\n",
        );
        let trees = Trees::of(&mirror, &base, &hwtest, &game(LINKS_RT)).unwrap();
        assert_ne!(trees.pinned_whole, trees.main_whole, "the whole tree moved");
        assert_eq!(trees.pinned, trees.main, "{}", trees.scope);
        assert!(
            trees.scope.starts_with("the SDK crates this game reaches"),
            "{}",
            trees.scope
        );
        // The game that builds the hardware tests (its Makefile names them) must repin.
        let builder = game(&[(
            "Makefile",
            "hwtest:\n\tcd $(PSOXIDE)/engine/examples/hardware-tests && cargo build\n",
        )]);
        let trees = Trees::of(&mirror, &base, &hwtest, &builder).unwrap();
        assert_ne!(trees.pinned, trees.main);
        // And so must one that builds every example through the SDK's makefile.
        std::fs::write(
            repo.path().join("tools/engine-examples.mk"),
            "hardware-tests:\n\tcd engine/examples/hardware-tests && cargo build\n",
        )
        .unwrap();
        util::git(repo.path(), &["add", "-A"]).unwrap();
        let with_mk = commit_edit(repo.path(), "sdk/psoxide.ld", "SECTIONS {}\n/* same */\n");
        let hwtest_again = commit_edit(
            repo.path(),
            "engine/examples/hardware-tests/src/main.rs",
            "const SUITE_VERSION: &str = \"v2.27\";\n",
        );
        let make = game(&[("Makefile", "-include .psoxide/tools/engine-examples.mk\n")]);
        let trees = Trees::of(&mirror, &with_mk, &hwtest_again, &make).unwrap();
        assert_ne!(trees.pinned, trees.main);
    }

    #[test]
    fn a_change_to_a_crate_a_game_links_still_refuses_it() {
        let repo = sdk_repo();
        let mirror = Mirror {
            dir: repo.path().to_path_buf(),
            stale: None,
        };
        let base = util::git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let rt = commit_edit(
            repo.path(),
            "sdk/crates/psx-rt/src/lib.rs",
            "pub fn rt() { loop {} }\n",
        );
        let trees = Trees::of(&mirror, &base, &rt, &game(LINKS_RT)).unwrap();
        assert_ne!(trees.pinned, trees.main);
        // A file outside every crate (the linker script) counts for every game.
        let ld = commit_edit(repo.path(), "sdk/psoxide.ld", "SECTIONS { . = 0; }\n");
        let nothing = game(&[]);
        let trees = Trees::of(&mirror, &rt, &ld, &nothing).unwrap();
        assert_ne!(trees.pinned, trees.main);
        // So does a guest tool, which every build uses.
        let tool = commit_edit(
            repo.path(),
            "tools/psoxide-link/src/lib.rs",
            "pub fn link() { }\n",
        );
        let trees = Trees::of(&mirror, &ld, &tool, &nothing).unwrap();
        assert_ne!(trees.pinned, trees.main);
    }

    #[test]
    fn an_unknown_reach_compares_the_whole_tree() {
        let repo = sdk_repo();
        let mirror = Mirror {
            dir: repo.path().to_path_buf(),
            stale: None,
        };
        let base = util::git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let hwtest = commit_edit(
            repo.path(),
            "engine/examples/hardware-tests/src/main.rs",
            "const SUITE_VERSION: &str = \"v2.26\";\n",
        );
        let unknown = Scope::Whole("no git work tree".into());
        let trees = Trees::of(&mirror, &base, &hwtest, &unknown).unwrap();
        assert_ne!(trees.pinned, trees.main);
        assert!(trees.scope.starts_with("the whole guest tree"));
        // A reach the SDK cannot resolve (a git dependency on a package it lacks): whole too.
        let odd = game(&[(
            "Cargo.toml",
            "[dependencies]\nx = { git = \"https://github.com/EBonura/PSoXide\", package = \"psx-gone\" }\n",
        )]);
        let trees = Trees::of(&mirror, &base, &hwtest, &odd).unwrap();
        assert_ne!(trees.pinned, trees.main);
        assert!(trees.scope.contains("psx-gone"), "{}", trees.scope);
        // A game whose files cannot be listed (not a git work tree) is judged whole.
        let not_git = tempfile::tempdir().unwrap();
        assert!(matches!(scope::of_game(not_git.path()), Scope::Whole(_)));
    }

    /// Write `path` in `repo` and commit everything at `time` (seconds since the epoch, committer
    /// and author); the new commit.
    fn commit_at(repo: &Path, path: &str, text: &str, time: u64) -> String {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
        util::git(repo, &["add", "-A"]).unwrap();
        let date = format!("@{time} +0000");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c"])
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        util::git(repo, &["rev-parse", "HEAD"]).unwrap()
    }

    /// SDK history for the deferral: `base`, a guest move `one` `gap` hours before `main`, a
    /// host-only commit, then a second guest move `main` at [`MOVED`].
    struct History {
        repo: tempfile::TempDir,
        base: String,
        one: String,
        main: String,
    }

    const MOVED: u64 = 1_795_000_000;
    const HOUR: u64 = 3600;

    fn history(gap: u64) -> History {
        let repo = sdk_repo();
        let r = repo.path();
        let base = util::git(r, &["rev-parse", "HEAD"]).unwrap();
        let rt = "sdk/crates/psx-rt/src/lib.rs";
        let one = commit_at(r, rt, "pub fn rt() { 1; }\n", MOVED - gap * HOUR);
        let host = "tools/psoxide-test/src/main.rs";
        commit_at(r, host, "fn main() {}\n", MOVED - HOUR);
        let main = commit_at(r, rt, "pub fn rt() { 2; }\n", MOVED);
        History {
            repo,
            base,
            one,
            main,
        }
    }

    impl History {
        fn mirror(&self) -> Mirror {
            Mirror {
                dir: self.repo.path().to_path_buf(),
                stale: None,
            }
        }

        /// The verdict of a game pinned at `pinned` that links psx-rt, against main.
        fn verdict(&self, pinned: &str) -> Verdict {
            let pin = Pin {
                revision: pinned.to_string(),
                repository: "EBonura/PSoXide".into(),
                recorded: None,
                file: "components.lock.json".into(),
            };
            judge(&self.mirror(), &pin, &game(LINKS_RT), Some(&self.main)).unwrap()
        }

        fn defer(
            &self,
            pinned: &str,
            until: u64,
            profile: crate::stage::Profile,
            now: u64,
        ) -> Result<Deferred, String> {
            let defer = crate::game::SdkDefer {
                until: util::rfc3339_utc(until),
                reason: "resident RAM over budget on the new SDK".into(),
            };
            let verdict = self.verdict(pinned);
            assert_ne!(verdict.pinned_tree, verdict.main_tree);
            deferral(
                &self.mirror(),
                &verdict,
                &game(LINKS_RT),
                &defer,
                profile,
                now,
            )
        }
    }

    use crate::stage::Profile;

    #[test]
    fn a_defer_holds_one_move_behind_on_a_landing_run() {
        let h = history(50);
        let d = h
            .defer(&h.one, MOVED + 24 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap();
        // The host-only commit between the moves is not a move.
        assert_eq!((d.move_commit.as_str(), d.missed), (h.main.as_str(), 1));
        assert_eq!(d.move_time, util::rfc3339_utc(MOVED));
        assert_eq!(d.until, util::rfc3339_utc(MOVED + 24 * HOUR));
        // The last moment the window allows is allowed.
        let edge = MOVED + DEFER_WINDOW_SECONDS;
        assert!(h.defer(&h.one, edge, Profile::Landing, MOVED).is_ok());
    }

    #[test]
    fn a_defer_is_refused_two_moves_behind() {
        // The first move the pin lacks is 50 h old: no `until` both in the future and inside
        // its window exists.
        let h = history(50);
        let e = h
            .defer(&h.base, MOVED + 24 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap_err();
        assert!(e.contains("more than 48 h after"), "{e}");
        assert!(e.contains("(2 guest-moving commits behind)"), "{e}");
        let e = h
            .defer(&h.base, MOVED - 3 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap_err();
        assert!(e.starts_with("it expired"), "{e}");
    }

    #[test]
    fn a_landing_of_several_guest_commits_is_one_window() {
        // A lane's branch fast-forwarded as several commits: the window starts at the first.
        let h = history(3);
        let d = h
            .defer(&h.base, MOVED + 24 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap();
        assert_eq!((d.move_commit.as_str(), d.missed), (h.one.as_str(), 2));
        let e = h
            .defer(&h.base, MOVED + 46 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap_err();
        assert!(e.contains("more than 48 h after"), "{e}");
    }

    #[test]
    fn a_pin_off_main_is_not_deferrable() {
        let h = history(3);
        let r = h.repo.path();
        util::git(r, &["checkout", "-q", "-b", "side", &h.base]).unwrap();
        let side = commit_at(
            r,
            "sdk/crates/psx-rt/src/lib.rs",
            "pub fn rt() { 9; }\n",
            MOVED,
        );
        let e = h
            .defer(&side, MOVED + 24 * HOUR, Profile::Landing, MOVED + HOUR)
            .unwrap_err();
        assert!(
            e.starts_with("where the pin left main cannot be found"),
            "{e}"
        );
    }

    #[test]
    fn a_defer_is_refused_after_until() {
        let h = history(50);
        let until = MOVED + 24 * HOUR;
        let e = h.defer(&h.one, until, Profile::Landing, until).unwrap_err();
        assert!(e.starts_with("it expired at 2026-"), "{e}");
        assert!(e.contains("repin to main"), "{e}");
    }

    #[test]
    fn a_defer_ending_more_than_48_hours_after_the_move_is_refused() {
        let h = history(50);
        let until = MOVED + DEFER_WINDOW_SECONDS + 1;
        let e = h
            .defer(&h.one, until, Profile::Landing, MOVED + HOUR)
            .unwrap_err();
        assert!(e.contains("more than 48 h after"), "{e}");
    }

    #[test]
    fn a_defer_is_refused_on_the_install_profile() {
        let h = history(50);
        let e = h
            .defer(&h.one, MOVED + 24 * HOUR, Profile::Install, MOVED + HOUR)
            .unwrap_err();
        assert!(e.contains("--profile landing` only"), "{e}");
    }

    #[test]
    fn a_defer_needs_a_reason_and_a_utc_time() {
        let h = history(50);
        let verdict = h.verdict(&h.one);
        let try_defer = |until: &str, reason: &str| {
            let defer = crate::game::SdkDefer {
                until: until.into(),
                reason: reason.into(),
            };
            deferral(
                &h.mirror(),
                &verdict,
                &game(LINKS_RT),
                &defer,
                Profile::Landing,
                MOVED,
            )
            .unwrap_err()
        };
        assert!(try_defer("2026-11-23T00:00:00Z", "  ").contains("`reason` is empty"));
        assert!(try_defer("2026-11-23", "ram").contains("not an RFC 3339 UTC time"));
    }

    #[test]
    fn the_cache_slot_carries_the_logic() {
        let digest = logic_digest();
        assert_eq!(digest.len(), 16);
        assert_eq!(digest, logic_digest());
        // The sources hashed are this module's and the scope's own text.
        assert!(LOGIC[0].contains("fn logic_digest()"));
        assert!(LOGIC[1].contains("pub fn reached_crates("));
    }

    #[test]
    fn the_guest_tree_is_recorded_beside_the_revision() {
        let lock = "{\n  \"components\": {\n    \"sdk\": {\n      \"repository\": \"EBonura/PSoXide\",\n      \"revision\": \"abc\",\n      \"paths\": []\n    }\n  }\n}\n";
        let once = record_guest_tree(lock, "\"sdk\"", "t1").unwrap();
        let v: serde_json::Value = serde_json::from_str(&once).unwrap();
        assert_eq!(v["components"]["sdk"]["guest_tree"], "t1");
        assert_eq!(v["components"]["sdk"]["revision"], "abc");
        let twice = record_guest_tree(&once, "\"sdk\"", "t2").unwrap();
        let v: serde_json::Value = serde_json::from_str(&twice).unwrap();
        assert_eq!(v["components"]["sdk"]["guest_tree"], "t2");
        assert_eq!(twice.matches("guest_tree").count(), 1);
        let flat = "{\n  \"repository\": \"x\",\n  \"revision\": \"abc\"\n}\n";
        let v: serde_json::Value =
            serde_json::from_str(&record_guest_tree(flat, "{", "t").unwrap()).unwrap();
        assert_eq!(v["guest_tree"], "t");
    }

    const MANIFEST: &str = r#"
[workspace]
members = ["crates/psx-hw", "tools/psoxide-link", "tools/psoxide-test"]
exclude = ["sdk"]
[profile.release]
opt-level = 3
"#;
    const LOCK: &str = r#"
version = 4
[[package]]
name = "psx-hw"
version = "0.1.0"
[[package]]
name = "psoxide-link"
version = "0.1.0"
dependencies = ["psx-hw", "anyhow"]
[[package]]
name = "anyhow"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"
[[package]]
name = "psoxide-test"
version = "0.1.0"
dependencies = ["tempfile"]
[[package]]
name = "tempfile"
version = "3.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bb"
"#;

    fn names(m: &str) -> Option<String> {
        m.rsplit('/').next().map(str::to_string)
    }

    fn tree(listing: &str, manifest: &str, lock: &str) -> String {
        guest_tree_of(listing, Some(manifest), Some(lock), names).unwrap()
    }

    #[test]
    fn a_host_tool_commit_leaves_the_guest_tree_equal() {
        let listing = "100644 blob 1111\tsdk/crates/psx-rt/src/lib.rs\n";
        let base = tree(listing, MANIFEST, LOCK);
        // A new workspace member and a new dependency of a host tool: equal.
        let manifest = MANIFEST.replace(
            "\"tools/psoxide-test\"]",
            "\"tools/psoxide-test\", \"tools/psoxide-dev\"]",
        );
        let lock = LOCK.replace("checksum = \"bb\"", "checksum = \"cc\"");
        assert_eq!(tree(listing, &manifest, &lock), base);
        // Documentation: equal.
        let docs = format!("{listing}100644 blob 2222\tsdk/docs/PIN.md\n100644 blob 3333\tcrates/psx-hw/docs/x.txt\n");
        assert_eq!(tree(&docs, MANIFEST, LOCK), base);
    }

    #[test]
    fn a_guest_change_moves_the_guest_tree() {
        let listing = "100644 blob 1111\tsdk/crates/psx-rt/src/lib.rs\n";
        let base = tree(listing, MANIFEST, LOCK);
        let edited = "100644 blob 9999\tsdk/crates/psx-rt/src/lib.rs\n";
        assert_ne!(tree(edited, MANIFEST, LOCK), base);
        // A dependency of a guest-path tool moved.
        let lock = LOCK.replace("checksum = \"aa\"", "checksum = \"ab\"");
        assert_ne!(tree(listing, MANIFEST, &lock), base);
        // The release profile changed.
        let manifest = MANIFEST.replace("opt-level = 3", "opt-level = 2");
        assert_ne!(tree(listing, &manifest, LOCK), base);
    }

    #[test]
    fn the_three_lock_shapes_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        assert!(read(r, None).is_none());
        std::fs::write(
            r.join("sdk.lock.json"),
            r#"{"repository": "https://github.com/EBonura/PSoXide.git", "revision": "9bb3045b"}"#,
        )
        .unwrap();
        let pin = read(r, None).unwrap();
        assert_eq!(
            (pin.revision.as_str(), pin.repository.as_str()),
            ("9bb3045b", "EBonura/PSoXide")
        );
        std::fs::write(
            r.join("components.lock.json"),
            r#"{"components": {"sdk": {"repository": "EBonura/PSoXide", "revision": "0187c5f4", "guest_tree": "g"}}}"#,
        )
        .unwrap();
        let pin = read(r, None).unwrap();
        assert_eq!(
            (pin.revision.as_str(), pin.recorded.as_deref()),
            ("0187c5f4", Some("g"))
        );
        let script = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(script.path().join("psyq")).unwrap();
        std::fs::write(
            script.path().join("psyq/build.sh"),
            "rev=${PSOXIDE_REV:-1bddae723228ef5c70bbc39ed134527626724a7d}\n",
        )
        .unwrap();
        let game = Game::parse(
            "schema = 1\nname = \"g\"\nrepository = \"o/g\"\n[[build]]\nname = \"b\"\nout = \"o\"\n\
             commands = [[\"true\"]]\ndisc = \"g.cue\"\n[sdk]\nfile = \"psyq/build.sh\"\nafter = \"PSOXIDE_REV:-\"\n",
        )
        .unwrap();
        let pin = read(script.path(), Some(&game)).unwrap();
        assert_eq!(pin.revision, "1bddae723228ef5c70bbc39ed134527626724a7d");
        assert_eq!(pin.repository, "EBonura/PSoXide");
    }
}
