//! Carrying a committed sample profile across source edits.
//!
//! LLVM keys a sample by function name, then `line - the function's declaration line`
//! and discriminator, and an inlined callee by the same offset of its call site in the
//! caller plus its own name. Editing lines inside a function therefore moves every key
//! below the edit, and LLVM, which cannot tell, applies the counts to other code.
//!
//! `collect` and `stamp` write the source of a profile as comments (LLVM skips them):
//!
//! ```text
//! # psoxide-pgo source: <commit the tree was at>
//! # psoxide-pgo file: <git blob id> <path in the repository>
//! # psoxide-pgo func: <declaration line> <last line> <file number> <portable name>
//! ```
//!
//! A file is the blob git holds for it, so the profile names its exact text, and a
//! function is where the profiled build put its code. At build time a tree whose blobs
//! equal the recorded ones needs nothing (one `git hash-object`). For any other the
//! function's old lines are diffed against its new ones, the new place coming from the
//! build's own DWARF, and every key of the function, and of its callee under any caller,
//! moves to the line its code went to.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Node;

pub const SOURCE_TAG: &str = "# psoxide-pgo source: ";
pub const FILE_TAG: &str = "# psoxide-pgo file: ";
pub const FUNC_TAG: &str = "# psoxide-pgo func: ";

/// A function that holds more than this share of the profile's samples and cannot be
/// carried (gone from the build) stops the build.
const REFUSE_GONE_ABOVE: f64 = 0.01;
/// Lines of the new text read past where this build's code on a function ends. The profile
/// may carry lines the build has no code on (a closing brace its DWARF never names), and
/// they need their partners.
const TAIL: u32 = 16;
/// The most table cells one function's line alignment may use.
const MAX_CELLS: usize = 16_000_000;

/// Where a function sits in the source of one build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// The declaration file as the line table spells it.
    pub file: Option<String>,
    pub decl: u32,
    /// The last line any of its code, or any call it inlines, is attributed to.
    pub last: u32,
}

/// Every function of one build that has code, by name.
#[derive(Default)]
pub struct Facts(pub BTreeMap<String, Fact>);

impl Facts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note that code of `func` carries `line`.
    pub fn reach(&mut self, func: &crate::Func, line: u64) {
        let (Some(name), Ok(line)) = (&func.name, u32::try_from(line)) else {
            return;
        };
        if let Some(fact) = self.0.get_mut(name.as_str()) {
            fact.last = fact.last.max(line);
        } else if let Ok(decl) = u32::try_from(func.line) {
            self.0.insert(
                name.clone(),
                Fact {
                    file: func.file.clone(),
                    decl,
                    last: line.max(decl),
                },
            );
        }
    }
}

/// The repository's side of the work: what a path is, and what text a blob held.
pub trait Tree {
    /// The tracked path (relative to the repository) a line table's file name is.
    fn rel(&self, raw: &str) -> Option<String>;
    /// The blob id of the working copy of each path; `None` for a file that is gone.
    /// `store` also writes the blob into the repository, so a text that was never
    /// committed can still be read back.
    fn blob_ids(&self, rels: &[String], store: bool) -> Result<Vec<Option<String>>, String>;
    /// The text of a blob a profile recorded.
    fn old(&self, id: &str) -> Result<String, String>;
    /// The text of the working copy of a tracked path.
    fn current(&self, rel: &str) -> Result<String, String>;
}

/// A git working tree.
pub struct Repo {
    top: PathBuf,
    roots: Vec<PathBuf>,
    tracked: std::collections::HashSet<String>,
    seen: std::cell::RefCell<HashMap<String, Option<String>>>,
}

impl Repo {
    /// The repository holding the first of `dirs`; relative names in a line table are
    /// tried against the repository root and every `dirs` entry (a workspace may be a
    /// subdirectory, and the line table's names are relative to it).
    pub fn open(dirs: &[&Path]) -> Option<Repo> {
        let first = dirs.first()?;
        let top = git_in(first, &["rev-parse", "--show-toplevel"], None).ok()?;
        let top = PathBuf::from(String::from_utf8(top).ok()?.trim());
        let top = top.canonicalize().ok()?;
        let listed = git_in(&top, &["ls-files", "-z"], None).ok()?;
        let tracked = listed
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .filter_map(|name| String::from_utf8(name.to_vec()).ok())
            .collect();
        let mut roots = vec![top.clone()];
        roots.extend(dirs.iter().filter_map(|dir| dir.canonicalize().ok()));
        Some(Repo {
            top,
            roots,
            tracked,
            seen: Default::default(),
        })
    }

    /// The commit the working tree is at, for the record.
    pub fn head(&self) -> String {
        git_in(&self.top, &["rev-parse", "HEAD"], None)
            .ok()
            .and_then(|out| String::from_utf8(out).ok())
            .map_or_else(|| "unknown".to_string(), |rev| rev.trim().to_string())
    }
}

fn git_in(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input).map_err(|e| format!("git: {e}"))?;
    }
    let output = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

impl Tree for Repo {
    fn rel(&self, raw: &str) -> Option<String> {
        if let Some(known) = self.seen.borrow().get(raw) {
            return known.clone();
        }
        let path = Path::new(raw);
        let candidates: Vec<PathBuf> = if path.is_absolute() {
            vec![path.to_path_buf()]
        } else {
            self.roots.iter().map(|root| root.join(path)).collect()
        };
        let found = candidates.iter().find_map(|candidate| {
            let inside = candidate.canonicalize().ok()?;
            let rel = inside.strip_prefix(&self.top).ok()?.to_str()?.to_string();
            self.tracked.contains(&rel).then_some(rel)
        });
        self.seen
            .borrow_mut()
            .insert(raw.to_string(), found.clone());
        found
    }

    fn blob_ids(&self, rels: &[String], store: bool) -> Result<Vec<Option<String>>, String> {
        let present: Vec<&String> = rels
            .iter()
            .filter(|rel| self.top.join(rel).is_file())
            .collect();
        let mut args = vec!["hash-object"];
        if store {
            args.push("-w");
        }
        args.push("--stdin-paths");
        let input: String = present.iter().map(|rel| format!("{rel}\n")).collect();
        let out = git_in(&self.top, &args, Some(input.as_bytes()))?;
        let mut ids = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
            .into_iter();
        Ok(rels
            .iter()
            .map(|rel| {
                if self.top.join(rel).is_file() {
                    ids.next()
                } else {
                    None
                }
            })
            .collect())
    }

    fn old(&self, id: &str) -> Result<String, String> {
        let bytes = git_in(&self.top, &["cat-file", "blob", id], None).map_err(|_| {
            format!(
                "this clone has no git blob {id}, the text the profile was collected from; fetch \
                 the history it came from (`git fetch --unshallow`), or collect the profile again"
            )
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn current(&self, rel: &str) -> Result<String, String> {
        std::fs::read(self.top.join(rel))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| format!("{rel}: {e}"))
    }
}

/// What a profile says about the tree it was made from.
#[derive(Debug, PartialEq)]
struct Stamp {
    files: Vec<(String, String)>,
    /// name -> (declaration line, last line, index into `files`)
    funcs: HashMap<String, (u32, u32, usize)>,
}

fn parse_stamp(text: &str) -> Option<Stamp> {
    let mut found = false;
    let mut files = Vec::new();
    let mut funcs = HashMap::new();
    for line in text.lines().take_while(|line| line.starts_with('#')) {
        if line.starts_with(SOURCE_TAG) {
            found = true;
        } else if let Some(rest) = line.strip_prefix(FILE_TAG) {
            let (id, path) = rest.split_once(' ')?;
            files.push((path.to_string(), id.to_string()));
        } else if let Some(rest) = line.strip_prefix(FUNC_TAG) {
            let mut fields = rest.splitn(4, ' ');
            let decl = fields.next()?.parse().ok()?;
            let last = fields.next()?.parse().ok()?;
            let file: usize = fields.next()?.parse().ok()?;
            if file >= files.len() {
                return None;
            }
            funcs.insert(fields.next()?.to_string(), (decl, last, file));
        }
    }
    found.then_some(Stamp { files, funcs })
}

/// The largest line offset the profile gives each function, wherever it appears.
fn deepest(funcs: &[(String, Node)]) -> HashMap<&str, u32> {
    fn walk<'a>(name: &'a str, node: &'a Node, out: &mut HashMap<&'a str, u32>) {
        let own = node.body.keys().map(|key| key.0);
        let sites = node.calls.keys().map(|key| key.0);
        // An offset of 0x8000 or more is a line above the declaration; it has no slice.
        let deepest = own
            .chain(sites)
            .filter(|offset| *offset < 0x8000)
            .max()
            .unwrap_or(0);
        let slot = out.entry(name).or_default();
        *slot = (*slot).max(deepest);
        for ((_, _, callee), child) in &node.calls {
            walk(callee, child, out);
        }
    }
    let mut out = HashMap::new();
    for (name, node) in funcs {
        walk(name, node, &mut out);
    }
    out
}

/// Every function name in a profile, top level or inlined.
fn names(funcs: &[(String, Node)]) -> BTreeSet<&str> {
    fn walk<'a>(node: &'a Node, out: &mut BTreeSet<&'a str>) {
        for ((_, _, callee), child) in &node.calls {
            out.insert(callee);
            walk(child, out);
        }
    }
    let mut out = BTreeSet::new();
    for (name, node) in funcs {
        out.insert(name.as_str());
        walk(node, &mut out);
    }
    out
}

fn is_record(line: &str) -> bool {
    [SOURCE_TAG, FILE_TAG, FUNC_TAG]
        .iter()
        .any(|tag| line.starts_with(tag))
}

/// The profile with its source record (re)written from `facts`, the functions of the
/// build the profile describes, keyed by portable name. The sample lines are untouched.
pub fn stamp_text(text: &str, facts: &Facts, tree: &dyn Tree, rev: &str) -> Result<String, String> {
    let (_, funcs) = parse_profile(text)?;
    let deepest = deepest(&funcs);
    let mut files: Vec<String> = Vec::new();
    let mut records: Vec<(String, u32, u32, String)> = Vec::new();
    for name in names(&funcs) {
        let Some(fact) = facts.0.get(name) else {
            continue;
        };
        let Some(rel) = fact.file.as_deref().and_then(|raw| tree.rel(raw)) else {
            continue;
        };
        // The profile may hold lines this build has no code on (it can be older than
        // the tree); they are still lines of the function.
        let last = fact.last.max(
            fact.decl
                .saturating_add(deepest.get(name).copied().unwrap_or(0)),
        );
        records.push((name.to_string(), fact.decl, last, rel.clone()));
        files.push(rel);
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err("no function of the profile is in a file this repository tracks".into());
    }
    let ids = tree.blob_ids(&files, true)?;
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    // Leading comments (the feature set) stay first.
    while let Some(line) = lines.next_if(|line| line.starts_with('#')) {
        if !is_record(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&format!("{SOURCE_TAG}{rev}\n"));
    for (rel, id) in files.iter().zip(&ids) {
        let id = id.as_deref().ok_or_else(|| format!("{rel} is gone"))?;
        out.push_str(&format!("{FILE_TAG}{id} {rel}\n"));
    }
    for (name, decl, last, rel) in &records {
        let file = files
            .binary_search(rel)
            .expect("recorded files hold every function's file");
        out.push_str(&format!("{FUNC_TAG}{decl} {last} {file} {name}\n"));
    }
    for line in lines.filter(|line| !is_record(line)) {
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// The comments of a profile, and its functions in file order.
type Parsed<'a> = (Vec<&'a str>, Vec<(String, Node)>);

fn parse_profile(text: &str) -> Result<Parsed<'_>, String> {
    let comments = text.lines().filter(|line| line.starts_with('#')).collect();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();
    let mut funcs = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        let mut fields = line.rsplitn(3, ':');
        let (Some(head), Some(_total), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("not a profile header: {line}"));
        };
        if line.starts_with(' ') {
            return Err(format!("a profile line outside a function: {line}"));
        }
        at += 1;
        let mut node = parse_block(&lines, &mut at, 1)?;
        node.head = head
            .parse()
            .map_err(|_| format!("not a profile header: {line}"))?;
        funcs.push((name.to_string(), node));
    }
    Ok((comments, funcs))
}

fn parse_block(lines: &[&str], at: &mut usize, depth: usize) -> Result<Node, String> {
    let mut node = Node::default();
    while let Some(line) = lines.get(*at) {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent < depth {
            break;
        }
        if indent > depth {
            return Err(format!("unexpected indent: {line}"));
        }
        let bad = || format!("not a profile line: {line}");
        let (location, rest) = line.trim_start().split_once(": ").ok_or_else(bad)?;
        let (offset, disc) = location.split_once('.').unwrap_or((location, "0"));
        let key = (
            offset.parse::<u32>().map_err(|_| bad())?,
            disc.parse::<u32>().map_err(|_| bad())?,
        );
        *at += 1;
        match rest.parse::<u64>() {
            Ok(count) => {
                let slot = node.body.entry(key).or_default();
                *slot = (*slot).max(count);
            }
            Err(_) => {
                let (callee, _) = rest.rsplit_once(':').ok_or_else(bad)?;
                let child = parse_block(lines, at, depth + 1)?;
                node.calls.insert((key.0, key.1, callee.to_string()), child);
            }
        }
    }
    Ok(node)
}

/// Where each old line offset of a function went; `None` for a line that is gone.
enum LineMap {
    Same,
    /// The function does not exist in this build.
    Gone,
    Offsets(Vec<Option<u32>>),
}

impl LineMap {
    /// Whether any line offset of the function ends up elsewhere, or nowhere.
    fn moves_lines(&self) -> bool {
        match self {
            LineMap::Offsets(map) => map
                .iter()
                .enumerate()
                .any(|(at, to)| *to != Some(at as u32)),
            LineMap::Same | LineMap::Gone => false,
        }
    }

    fn apply(&self, offset: u32) -> Option<u32> {
        match self {
            LineMap::Same | LineMap::Gone => Some(offset),
            LineMap::Offsets(map) => map.get(offset as usize).copied().flatten(),
        }
    }
}

/// What carrying a profile did.
#[derive(Default, Debug)]
pub struct Report {
    pub files: usize,
    /// Functions (top level or inlined) with a line that is elsewhere or gone.
    pub moved: usize,
    pub dropped_samples: u64,
    /// The functions that lost the most samples to deleted lines, most first.
    pub dropped_in: Vec<(String, u64)>,
    pub total_samples: u64,
    /// Functions of the profile that this build does not have, with their samples.
    pub gone: Vec<(String, u64)>,
}

impl Report {
    fn gone_samples(&self) -> u64 {
        self.gone.iter().map(|gone| gone.1).sum()
    }

    /// Why the build must stop, if it must: a function that carries more than
    /// [`REFUSE_GONE_ABOVE`] of the samples is not in this build, so its counts cannot
    /// reach any code (renamed, or removed).
    pub fn refusal(&self) -> Option<String> {
        let hot: Vec<String> = self
            .gone
            .iter()
            .filter(|(_, samples)| *samples as f64 > REFUSE_GONE_ABOVE * self.total_samples as f64)
            .map(|(name, samples)| {
                format!(
                    "{name} ({:.1}%)",
                    100.0 * *samples as f64 / self.total_samples as f64
                )
            })
            .collect();
        (!hot.is_empty()).then(|| {
            format!(
                "the profile's hot functions are not in this build, so no edit of lines can \
                 carry their counts: {}. A rename or a move to another crate needs `psoxide-pgo \
                 collect` again",
                hot.join(", ")
            )
        })
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let share = |part: u64| 100.0 * part as f64 / self.total_samples.max(1) as f64;
        write!(
            f,
            "profile carried across edits to {} file(s): {} function(s) moved their lines, \
             {} samples ({:.2}%) sat on lines that are gone",
            self.files,
            self.moved,
            self.dropped_samples,
            share(self.dropped_samples)
        )?;
        let mut lost = self.dropped_in.clone();
        lost.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        lost.dedup_by(|a, b| a.0 == b.0);
        if let Some((name, samples)) = lost.first() {
            write!(f, " (most in {name}: {samples})")?;
        }
        if !self.gone.is_empty() {
            write!(
                f,
                "; {} function(s) are not in this build and keep their old keys ({:.2}% of the samples)",
                self.gone.len(),
                share(self.gone_samples())
            )?;
        }
        Ok(())
    }
}

/// Carry a profile from the tree it was made from to the tree `new` describes (the
/// functions of the build about to use it, by portable name).
///
/// `Err` carries a message that says what to run. `Ok` has the profile text (the input
/// itself when every recorded file is as it was) and the report.
pub fn carry(text: &str, new: &Facts, tree: &dyn Tree) -> Result<(String, Report), String> {
    let Some(stamp) = parse_stamp(text) else {
        return Err(
            "the profile does not say what source it was collected from. If it describes this \
             tree, record that with `psoxide-pgo stamp --crate GAME --profile PROFILE -- \
             CARGO-ARGS` (the same arguments as the game's build); otherwise collect it again"
                .into(),
        );
    };
    let rels: Vec<String> = stamp.files.iter().map(|file| file.0.clone()).collect();
    let now = tree.blob_ids(&rels, false)?;
    let same: Vec<bool> = stamp
        .files
        .iter()
        .zip(&now)
        .map(|((_, recorded), now)| now.as_deref() == Some(recorded.as_str()))
        .collect();
    if same.iter().all(|&same| same) {
        return Ok((text.to_string(), Report::default()));
    }

    let (comments, funcs) = parse_profile(text)?;
    let mut carrier = Carrier {
        stamp: &stamp,
        same: &same,
        new,
        tree,
        old_text: HashMap::new(),
        new_text: HashMap::new(),
        maps: HashMap::new(),
        report: Report {
            files: same.iter().filter(|&&same| !same).count(),
            ..Report::default()
        },
        error: None,
    };
    let mut out = String::new();
    for line in comments {
        out.push_str(line);
        out.push('\n');
    }
    for (name, node) in funcs {
        carrier.report.total_samples += node.total();
        let node = carrier.node(&name, node, true);
        let _ = writeln!(out, "{name}:{}:{}", node.total(), node.head);
        node.emit(1, &mut out);
    }
    if let Some(error) = carrier.error {
        return Err(error);
    }
    let mut report = carrier.report;
    report
        .gone
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok((out, report))
}

struct Carrier<'a> {
    stamp: &'a Stamp,
    same: &'a [bool],
    new: &'a Facts,
    tree: &'a dyn Tree,
    old_text: HashMap<usize, String>,
    new_text: HashMap<String, String>,
    maps: HashMap<String, LineMap>,
    report: Report,
    error: Option<String>,
}

impl Carrier<'_> {
    /// `name`'s node with every key moved to where its line went.
    fn node(&mut self, name: &str, node: Node, top: bool) -> Node {
        if !self.maps.contains_key(name) {
            let map = self.plan(name).unwrap_or_else(|error| {
                self.error.get_or_insert(error);
                LineMap::Same
            });
            self.report.moved += usize::from(map.moves_lines());
            self.maps.insert(name.to_string(), map);
        }
        if matches!(self.maps[name], LineMap::Gone) {
            // Nothing in this build has the name, so LLVM never looks these keys up.
            let samples = node.total();
            if top || !self.report.gone.iter().any(|gone| gone.0 == name) {
                self.report.gone.push((name.to_string(), samples));
            }
            return node;
        }
        let mut out = Node {
            head: node.head,
            ..Node::default()
        };
        let mut dropped = 0;
        for ((offset, disc), count) in node.body {
            match self.maps[name].apply(offset) {
                Some(moved) => {
                    let slot = out.body.entry((moved, disc)).or_default();
                    *slot = (*slot).max(count);
                }
                None => dropped += count,
            }
        }
        for ((offset, disc, callee), child) in node.calls {
            let child = self.node(&callee, child, false);
            match self.maps[name].apply(offset) {
                Some(moved) => merge(out.calls.entry((moved, disc, callee)).or_default(), child),
                None => dropped += child.total(),
            }
        }
        self.report.dropped_samples += dropped;
        if dropped > 0 {
            self.report.dropped_in.push((name.to_string(), dropped));
        }
        out
    }

    fn plan(&mut self, name: &str) -> Result<LineMap, String> {
        let Some(&(decl, last, file)) = self.stamp.funcs.get(name) else {
            // Not in a file of the repository (the SDK, the standard library): its
            // text is not ours to diff and its keys stay as they were.
            return Ok(LineMap::Same);
        };
        let Some(site) = self.new.0.get(name) else {
            return Ok(LineMap::Gone);
        };
        let Some(rel) = site.file.as_deref().and_then(|raw| self.tree.rel(raw)) else {
            return Ok(LineMap::Gone);
        };
        let (old_rel, old_id) = &self.stamp.files[file];
        if *old_rel == rel && self.same[file] && site.decl == decl {
            return Ok(LineMap::Same);
        }
        if !self.old_text.contains_key(&file) {
            let text = self.tree.old(old_id)?;
            self.old_text.insert(file, text);
        }
        if !self.new_text.contains_key(&rel) {
            let text = self.tree.current(&rel)?;
            self.new_text.insert(rel.clone(), text);
        }
        let slice = |text: &'_ str, from: u32, to: u32| -> Vec<String> {
            text.lines()
                .skip(from.saturating_sub(1) as usize)
                .take((to.max(from) - from + 1) as usize)
                .map(|line| line.trim().to_string())
                .collect()
        };
        let old = slice(&self.old_text[&file], decl, last);
        let new = slice(
            &self.new_text[&rel],
            site.decl,
            site.last.saturating_add(TAIL),
        );
        if old.is_empty() || new.is_empty() {
            return Ok(LineMap::Gone);
        }

        align(&old, &new)
            .map(|pairs| {
                LineMap::Offsets(pairs.into_iter().map(|to| to.map(|n| n as u32)).collect())
            })
            .map_err(|why| format!("{name}: {why}; collect the profile again"))
    }
}

/// `into` plus `other`: the larger count of a line, the union of the call sites.
fn merge(into: &mut Node, other: Node) {
    into.head = into.head.max(other.head);
    for (key, count) in other.body {
        let slot = into.body.entry(key).or_default();
        *slot = (*slot).max(count);
    }
    for (key, child) in other.calls {
        merge(into.calls.entry(key).or_default(), child);
    }
}

/// For each line of `old`, the index of the line of `new` it became. Equal lines (ignoring
/// indentation, which the caller trims) pair up as a longest common subsequence; between
/// two pairs, the lines of a run that was rewritten pair by position, and the rest of
/// either side has no partner: a deleted line takes its samples with it, an inserted one
/// has none.
fn align(old: &[String], new: &[String]) -> Result<Vec<Option<usize>>, String> {
    let (n, m) = (old.len(), new.len());
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (&old[prefix..n - suffix], &new[prefix..m - suffix]);
    let (rows, cols) = (a.len(), b.len());
    if (rows + 1) * (cols + 1) > MAX_CELLS {
        return Err(format!("{rows} lines against {cols} are too many to align"));
    }
    let width = cols + 1;
    let mut table = vec![0u32; (rows + 1) * width];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut pairs: Vec<(usize, usize)> = (0..prefix).map(|i| (i, i)).collect();
    let (mut i, mut j) = (0, 0);
    while i < rows && j < cols {
        if a[i] == b[j] {
            pairs.push((prefix + i, prefix + j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs.extend((0..suffix).map(|k| (n - suffix + k, m - suffix + k)));

    let mut map = vec![None; n];
    let mut edge = (0usize, 0usize);
    for &(i, j) in pairs.iter().chain(std::iter::once(&(n, m))) {
        // The lines between the previous pair and this one on each side.
        for k in 0..(i - edge.0).min(j - edge.1) {
            map[edge.0 + k] = Some(edge.1 + k);
        }
        if i < n {
            map[i] = Some(j);
        }
        edge = (i + 1, j + 1);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: &str = "game::step";
    const OLD: &str = "\
fn other() {
    a();
}

fn step() {
    let x = load();
    if x > 3 {
        hot(x);
    }
    done();
}
";

    /// A fake repository: files by path, blobs by id.
    #[derive(Default)]
    struct Fake {
        files: BTreeMap<String, String>,
        blobs: HashMap<String, String>,
    }

    fn id_of(text: &str) -> String {
        format!("{:016x}", crate::pipeline::fnv1a(text.as_bytes()))
    }

    impl Fake {
        fn with(files: &[(&str, &str)]) -> Fake {
            let mut fake = Fake::default();
            for (path, text) in files {
                fake.write(path, text);
            }
            fake
        }

        fn write(&mut self, path: &str, text: &str) {
            self.blobs.insert(id_of(text), text.to_string());
            self.files.insert(path.to_string(), text.to_string());
        }

        /// The tree after an edit: the same blobs known, other working files.
        fn edit(&self, files: &[(&str, &str)]) -> Fake {
            let mut next = Fake {
                files: self.files.clone(),
                blobs: self.blobs.clone(),
            };
            for (path, text) in files {
                next.files.insert(path.to_string(), text.to_string());
            }
            next
        }
    }

    impl Tree for Fake {
        fn rel(&self, raw: &str) -> Option<String> {
            self.files.contains_key(raw).then(|| raw.to_string())
        }
        fn blob_ids(&self, rels: &[String], _: bool) -> Result<Vec<Option<String>>, String> {
            Ok(rels
                .iter()
                .map(|rel| self.files.get(rel).map(|t| id_of(t)))
                .collect())
        }
        fn old(&self, id: &str) -> Result<String, String> {
            self.blobs
                .get(id)
                .cloned()
                .ok_or_else(|| format!("no blob {id}"))
        }
        fn current(&self, rel: &str) -> Result<String, String> {
            self.files
                .get(rel)
                .cloned()
                .ok_or_else(|| format!("no {rel}"))
        }
    }

    fn fact(file: &str, decl: u32, last: u32) -> Fact {
        Fact {
            file: Some(file.to_string()),
            decl,
            last,
        }
    }

    fn facts(entries: &[(&str, Fact)]) -> Facts {
        Facts(
            entries
                .iter()
                .map(|(name, fact)| (name.to_string(), fact.clone()))
                .collect(),
        )
    }

    /// `game::step` of OLD: declared at line 5, samples at offsets 1, 2, 3 (lines 6, 7, 8).
    const PROFILE: &str = "# psoxide-pgo features: (default)\n\
        game::step:300:2\n 1: 100\n 2: 100\n 3: 90\n 5: 10\n";

    fn stamped(tree: &Fake, facts: &Facts) -> String {
        stamp_text(PROFILE, facts, tree, "abc123").expect("stamp")
    }

    fn body(text: &str) -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_tree_that_matches_is_not_touched() {
        let tree = Fake::with(&[("g.rs", OLD)]);
        let facts = facts(&[(F, fact("g.rs", 5, 10))]);
        let text = stamped(&tree, &facts);
        assert!(
            text.starts_with("# psoxide-pgo features: (default)\n# psoxide-pgo source: abc123\n")
        );
        let (carried, report) = carry(&text, &facts, &tree).expect("carry");
        assert_eq!(carried, text);
        assert_eq!(report.files, 0);
    }

    #[test]
    fn a_line_the_build_has_no_code_on_is_still_a_line_of_the_function() {
        // The profile has samples at offset 6 (the closing brace), the build's code ends
        // at line 10 (offset 5). The record covers offset 6, and an insertion moves it.
        let profile = PROFILE.replace(" 5: 10\n", " 5: 10\n 6: 7\n");
        let tree = Fake::with(&[("g.rs", OLD)]);
        let facts = facts(&[(F, fact("g.rs", 5, 10))]);
        let text = stamp_text(&profile, &facts, &tree, "r").expect("stamp");
        assert_eq!(parse_stamp(&text).expect("stamp").funcs[F], (5, 11, 0));
        let edited = OLD.replace("    done();", "    again();\n    done();");
        let tree = tree.edit(&[("g.rs", &edited)]);
        let (carried, _) =
            carry(&text, &self::facts(&[(F, fact("g.rs", 5, 12))]), &tree).expect("carry");
        assert!(body(&carried).contains(&" 7: 7".to_string()), "{carried}");
    }

    #[test]
    fn stamping_twice_changes_nothing() {
        let tree = Fake::with(&[("g.rs", OLD)]);
        let facts = facts(&[(F, fact("g.rs", 5, 10))]);
        let once = stamped(&tree, &facts);
        let twice = stamp_text(&once, &facts, &tree, "abc123").expect("stamp");
        assert_eq!(once, twice);
        assert_eq!(parse_stamp(&once).expect("stamp").funcs[F], (5, 10, 0));
    }

    #[test]
    fn an_unstamped_profile_says_what_to_run() {
        let tree = Fake::with(&[("g.rs", OLD)]);
        let error = carry(PROFILE, &Facts::new(), &tree).expect_err("unstamped");
        assert!(error.contains("psoxide-pgo stamp"), "{error}");
    }

    #[test]
    fn pure_insertion_moves_the_keys_below_it() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        // Two lines inserted after `let x`, above the hot line.
        let new_src = OLD.replace(
            "    if x > 3 {",
            "    let y = 1;\n    let z = 2;\n    if x > 3 {",
        );
        let tree = Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", &new_src)]);
        let facts = facts(&[(F, fact("g.rs", 5, 12))]);
        let (carried, report) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            ["game::step:300:2", " 1: 100", " 4: 100", " 5: 90", " 7: 10"]
        );
        assert_eq!(report.dropped_samples, 0);
    }

    #[test]
    fn pure_deletion_drops_the_deleted_lines_and_moves_the_rest() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        // `if x > 3 {` (offset 2) deleted along with its closing brace's partner line.
        let new_src = OLD.replace("    if x > 3 {\n", "");
        let tree = Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", &new_src)]);
        let facts = facts(&[(F, fact("g.rs", 5, 9))]);
        let (carried, report) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            ["game::step:200:2", " 1: 100", " 2: 90", " 4: 10"]
        );
        assert_eq!(report.dropped_samples, 100);
    }

    #[test]
    fn a_changed_line_keeps_its_samples() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        let new_src = OLD.replace("if x > 3 {", "if x > 7 {");
        let tree = Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", &new_src)]);
        let facts = facts(&[(F, fact("g.rs", 5, 10))]);
        let (carried, _) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            ["game::step:300:2", " 1: 100", " 2: 100", " 3: 90", " 5: 10"]
        );
    }

    #[test]
    fn a_moved_function_keeps_its_offsets() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        // `step` now follows twenty lines of something else, in the same file.
        let new_src = format!("{}{}", "// padding\n".repeat(20), OLD);
        let tree = Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", &new_src)]);
        let facts = facts(&[(F, fact("g.rs", 25, 30))]);
        let (carried, report) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            ["game::step:300:2", " 1: 100", " 2: 100", " 3: 90", " 5: 10"]
        );
        assert_eq!(report.dropped_samples, 0);
    }

    #[test]
    fn a_function_moved_to_another_file_keeps_its_offsets() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        let moved = "use x;\n\nfn step() {\n    let x = load();\n    if x > 3 {\n        hot(x);\n    }\n    done();\n}\n";
        let tree =
            Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", "fn other() {\n}\n"), ("h.rs", moved)]);
        let facts = facts(&[(F, fact("h.rs", 3, 8))]);
        let (carried, _) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            ["game::step:300:2", " 1: 100", " 2: 100", " 3: 90", " 5: 10"]
        );
    }

    /// A caller at line 1 with the callee inlined at offset 2, the callee declared at line 10.
    const INLINE_SRC: &str = "\
fn caller() {
    prep();
    helper();
    tail();
}
// a gap
// a gap
// a gap
// a gap
fn helper() {
    let a = 1;
    let b = 2;
    work(a, b);
}
";
    const INLINE_PROFILE: &str = "# psoxide-pgo features: (default)\n\
        game::caller:60:5\n 1: 10\n 2: game::helper:40\n  1: 10\n  3: 30\n 3: 10\n\
        game::helper:40:0\n 1: 10\n 3: 30\n";

    fn inline_facts(helper_last: u32) -> Facts {
        facts(&[
            ("game::caller", fact("g.rs", 1, 5)),
            ("game::helper", fact("g.rs", 10, helper_last)),
        ])
    }

    #[test]
    fn an_edit_inside_an_inlined_callee_moves_its_keys_under_every_caller() {
        let tree = Fake::with(&[("g.rs", INLINE_SRC)]);
        let text = stamp_text(INLINE_PROFILE, &inline_facts(14), &tree, "r").expect("stamp");
        // A line inserted at the top of the callee's body: `work` moves from offset 3 to 4.
        let edited = INLINE_SRC.replace("    let a = 1;", "    trace();\n    let a = 1;");
        let tree = tree.edit(&[("g.rs", &edited)]);
        let (carried, report) = carry(&text, &inline_facts(15), &tree).expect("carry");
        assert_eq!(
            body(&carried),
            [
                "game::caller:60:5",
                " 1: 10",
                " 2: game::helper:40",
                "  2: 10",
                "  4: 30",
                " 3: 10",
                "game::helper:40:0",
                " 2: 10",
                " 4: 30"
            ]
        );
        // The callee's offsets 1 and 3 are `let a` and `work`; the call site did not move.
        assert_eq!((report.files, report.dropped_samples), (1, 0));
    }

    #[test]
    fn an_edit_inside_an_inlined_callee_and_one_in_its_caller_both_apply() {
        let tree = Fake::with(&[("g.rs", INLINE_SRC)]);
        let text = stamp_text(INLINE_PROFILE, &inline_facts(14), &tree, "r").expect("stamp");
        // The callee gains a line at the top of its body, and the caller one before its call.
        let edited = INLINE_SRC
            .replace("    let a = 1;", "    trace();\n    let a = 1;")
            .replace("    helper();", "    log();\n    helper();");
        let tree = tree.edit(&[("g.rs", &edited)]);
        let facts = facts(&[
            ("game::caller", fact("g.rs", 1, 6)),
            ("game::helper", fact("g.rs", 11, 16)),
        ]);
        let (carried, _) = carry(&text, &facts, &tree).expect("carry");
        assert_eq!(
            body(&carried),
            [
                "game::caller:60:5",
                " 1: 10",
                " 3: game::helper:40",
                "  2: 10",
                "  4: 30",
                " 4: 10",
                "game::helper:40:0",
                " 2: 10",
                " 4: 30"
            ]
        );
    }

    #[test]
    fn a_renamed_function_keeps_its_keys_and_is_reported() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        let new_src = OLD.replace("fn step()", "fn stride()");
        let tree = Fake::with(&[("g.rs", OLD)]).edit(&[("g.rs", &new_src)]);
        // The build has `game::stride` and no `game::step`.
        let facts = facts(&[("game::stride", fact("g.rs", 5, 10))]);
        let (carried, report) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(body(&carried), body(&old));
        assert_eq!(report.gone, [(F.to_string(), 300)]);
        // All of the profile is in it, so the build stops.
        assert!(report
            .refusal()
            .expect("refused")
            .contains("game::step (100.0%)"));
    }

    #[test]
    fn a_cold_renamed_function_does_not_stop_the_build() {
        let mut old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        old.push_str("game::other:50000:0\n 1: 50000\n");
        let tree = Fake::with(&[("g.rs", OLD)])
            .edit(&[("g.rs", &OLD.replace("fn step()", "fn stride()"))]);
        let facts = facts(&[("game::stride", fact("g.rs", 5, 10))]);
        let (_, report) = carry(&old, &facts, &tree).expect("carry");
        assert_eq!(report.gone.len(), 1);
        assert!(report.refusal().is_none(), "{report}");
    }

    #[test]
    fn a_blob_the_clone_lacks_says_what_to_fetch() {
        let old = stamped(
            &Fake::with(&[("g.rs", OLD)]),
            &facts(&[(F, fact("g.rs", 5, 10))]),
        );
        let mut tree = Fake::with(&[("g.rs", &OLD.replace("done();", "done(); done();"))]);
        tree.blobs.clear();
        tree.blobs.insert("x".into(), "y".into());
        let error = carry(&old, &facts(&[(F, fact("g.rs", 5, 10))]), &tree).expect_err("no blob");
        assert!(error.contains("no blob"), "{error}");
    }

    #[test]
    fn the_alignment_pairs_equal_lines_and_rewritten_runs() {
        let a: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let b: Vec<String> = ["a", "x", "y", "d"].map(String::from).to_vec();
        assert_eq!(
            align(&a, &b).expect("align"),
            [Some(0), Some(1), Some(2), Some(3)]
        );
        let b: Vec<String> = ["a", "d"].map(String::from).to_vec();
        assert_eq!(
            align(&a, &b).expect("align"),
            [Some(0), None, None, Some(1)]
        );
        let b: Vec<String> = ["a", "q", "b", "c", "d"].map(String::from).to_vec();
        assert_eq!(
            align(&a, &b).expect("align"),
            [Some(0), Some(2), Some(3), Some(4)]
        );
    }

    #[test]
    fn profiles_round_trip_through_the_parser() {
        let (_, funcs) = parse_profile(INLINE_PROFILE).expect("parse");
        let mut out = String::new();
        for (name, node) in &funcs {
            out.push_str(&format!("{name}:{}:{}\n", node.total(), node.head));
            node.emit(1, &mut out);
        }
        assert_eq!(
            out,
            INLINE_PROFILE
                .lines()
                .skip(1)
                .map(|l| format!("{l}\n"))
                .collect::<String>()
        );
    }
}
