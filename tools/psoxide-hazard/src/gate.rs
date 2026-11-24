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
//! * a call of a deprecated SDK entry point;
//! * with `--gpu-words`, a raw GP0 or GP1 command word (`gpu-word`, see [`gpu_word_in`]).
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

/// Whether `v` reads as a GP0 or GP1 command word: a command byte in the top eight bits and a
/// payload that fits that command. The payload test is what keeps hash primes (`0x0100_0193`),
/// flag bits (`1 << 24`, `0x8000_0000`, `0x4000_0000`), KSEG bases (`0xA000_0000`) and GTE command
/// words (`0x4A18_0001`) out: a constant that happens to start with an `E1` byte almost never has
/// the bits above the command's payload clear. Commands covered: GP1 `02` to `09` (acknowledge,
/// display enable, DMA direction, display start, horizontal and vertical range, display mode,
/// texture disable), GP0 `1F` (IRQ), `E1` to `E6` (draw mode, window, area, offset, mask), and the
/// bare primitive and copy commands with no payload (`20`..`7F` that are not a single bit).
fn gpu_word(v: u32) -> bool {
    let (top, payload) = ((v >> 24) as u8, v & 0x00ff_ffff);
    match top {
        0x02 | 0x1f => payload == 0,
        0x03 | 0x09 => payload < 2,
        0x04 => payload < 4,
        0x05 => payload < 1 << 19,
        0x06 | 0x07 => payload < 1 << 20,
        0x08 => payload < 0x100,
        0xe1 => payload < 1 << 16,
        0xe2..=0xe4 => payload < 1 << 20,
        0xe5 => payload < 1 << 22,
        0xe6 => payload < 4,
        0x20..=0x7f => payload == 0 && !top.is_power_of_two(),
        _ => false,
    }
}

/// The command byte of a `<< 24` form: `0xE1 << 24`, `(0x30u32) << 24`, `225 << 24`.
fn gpu_command_byte(v: u64) -> bool {
    matches!(v, 0x1f | 0x20..=0x7f | 0x80 | 0xa0 | 0xc0 | 0xe1..=0xe6)
}

/// Whether a line of code (comments blanked) holds a raw GP0 or GP1 command word, as an
/// 8-digit hex literal (`0xE100_0200`), a decimal literal of the same value (`3774874112`) or a
/// command byte shifted up by 24 (`0xE1 << 24`, `0xA0u32 << 24`). See [`gpu_word`] for what
/// counts and what does not.
///
/// # Panics
///
/// Never: the patterns are fixed and valid.
pub fn gpu_word_in(code: &str) -> bool {
    static PATTERNS: std::sync::OnceLock<[Regex; 3]> = std::sync::OnceLock::new();
    let [hex, decimal, shifted] = PATTERNS.get_or_init(|| {
        [
            Regex::new(r"\b0[xX]([0-9a-fA-F_]+)").expect("valid"),
            Regex::new(r"(?:^|[^\w.])(\d[\d_]{6,})(?:u32|i32|u64|i64|usize)?\b(?:[^.]|$)").expect("valid"),
            Regex::new(r"\b(0[xX][0-9a-fA-F_]+|\d[\d_]*)(?:u8|u16|u32|i32|u64|i64|usize)?\s*\)?\s*<<\s*24\b")
                .expect("valid"),
        ]
    });
    let digits = |s: &str| s.chars().filter(|c| *c != '_').collect::<String>();
    let hit_hex = hex.captures_iter(code).any(|c| {
        let d = digits(&c[1]);
        // An integer suffix after the digits (`0xE1000000u32`) is not a hex digit; `_` was dropped.
        d.len() == 8 && u32::from_str_radix(&d, 16).is_ok_and(gpu_word)
    });
    let hit_decimal = decimal.captures_iter(code).any(|c| {
        digits(&c[1])
            .parse::<u32>()
            .is_ok_and(|v| v > 0x00ff_ffff && gpu_word(v))
    });
    let hit_shift = shifted.captures_iter(code).any(|c| {
        let text = digits(&c[1]);
        let value = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            Some(h) => u64::from_str_radix(h, 16),
            None => text.parse(),
        };
        value.is_ok_and(gpu_command_byte)
    });
    hit_hex || hit_decimal || hit_shift
}

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
        "gpu-word" => {
            "sdk/docs/GATE.md rule 3 (a game does not write a GPU command word by hand); use the \
             psx_hw::gpu constants and builders (or psx_gpu's typed commands)"
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
/// Lines (0-based) of `lines` that use a deprecated item written `krate::name` in the profile.
///
/// A line is a use when it names the path (`krate::name`), when it imports the item
/// (`use krate::{..., name, ...}`), or, in a file that imports it or `krate::*`, when it calls the bare
/// name as a free function: a definition (`fn name`) and a method call (`.name(`) are the program's own
/// items and pass. An alias (`name as other`) is not followed.
fn qualified_hits(lines: &[String], items: &[(String, String)]) -> BTreeSet<usize> {
    let mut hits = BTreeSet::new();
    for (krate, name) in items {
        let path = Regex::new(&format!(
            r"\b{}::{}\b",
            regex::escape(krate),
            regex::escape(name)
        ))
        .expect("escaped");
        let bare = Regex::new(&format!(r"(^|[^.\w:]){}\b", regex::escape(name))).expect("escaped");
        let word = Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("escaped");
        let defined = Regex::new(&format!(r"\bfn\s+{}\b", regex::escape(name))).expect("escaped");
        let aliased = Regex::new(&format!(r"\b{}\s+as\b", regex::escape(name))).expect("escaped");
        let starts = Regex::new(&format!(
            r"^\s*(pub(\([^)]*\))?\s+)?use\s+{}::",
            regex::escape(krate)
        ))
        .expect("escaped");
        let mut imported = false;
        let mut in_use = false;
        let mut use_lines = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            if starts.is_match(line) {
                in_use = true;
            }
            if in_use {
                if word.is_match(line) && !aliased.is_match(line) {
                    imported = true;
                    hits.insert(n);
                }
                if line.contains('*') {
                    imported = true;
                }
                use_lines.push(n);
                if line.contains(';') {
                    in_use = false;
                }
            } else if path.is_match(line) {
                hits.insert(n);
            }
        }
        if imported {
            for (n, line) in lines.iter().enumerate() {
                if !use_lines.contains(&n) && bare.is_match(line) && !defined.is_match(line) {
                    hits.insert(n);
                }
            }
        }
    }
    hits
}

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

/// A `static`/`const` whose type is an array (or slice) of an 8- or 16-bit integer, up to its `=`.
/// Such an element cannot hold a hardware address (the window needs 29 bits), so a `0x1F80` inside
/// the initializer is a data halfword, not a register.
const DATA_TABLE: &str = r"^\s*(pub(\([^)]*\))?\s+)?(static|const)\s+(mut\s+)?\w+\s*:\s*&?\s*('\w+\s+)?(\[\s*)+(u8|i8|u16|i16)\b[^=]*=";

/// `lines` with the initializers of 8- and 16-bit integer table items blanked, for the
/// register-literal rule only (the other rules still read the original lines). The initializer
/// runs from the `=` to the `;` at bracket depth 0; an item with no such end blanks nothing.
fn mask_data_tables(lines: &[String]) -> Vec<String> {
    let decl = Regex::new(DATA_TABLE).expect("valid");
    let mut out: Vec<String> = lines.to_vec();
    let mut n = 0;
    while n < lines.len() {
        let Some(found) = decl.find(&lines[n]) else {
            n += 1;
            continue;
        };
        let mut depth = 0i32;
        let mut end = None;
        'scan: for (m, line) in lines.iter().enumerate().skip(n) {
            let from = if m == n { found.end() } else { 0 };
            for (i, c) in line.char_indices().filter(|(i, _)| *i >= from) {
                match c {
                    '[' | '(' | '{' => depth += 1,
                    ']' | ')' | '}' => depth -= 1,
                    ';' if depth <= 0 => {
                        end = Some((m, i));
                        break 'scan;
                    }
                    _ => {}
                }
            }
        }
        let Some((last, stop)) = end else {
            n += 1;
            continue;
        };
        for m in n..=last {
            let from = if m == n { found.end() } else { 0 };
            let to = if m == last { stop } else { lines[m].len() };
            out[m] = format!(
                "{}{}{}",
                &lines[m][..from],
                " ".repeat(to - from),
                &lines[m][to..]
            );
        }
        n = last + 1;
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

/// The SDK checkout the program links, from where its `psx-rt` lives: the nearest directory above
/// it that holds `sdk/profile.toml`. A tool's own SDK (the one it was built from) can be another
/// checkout: a copied or APFS-cloned game repository carries its hydrated `.psoxide` SDK with the
/// tools built in the original, whose compile-time root then names the original's files, and
/// every SDK crate of the clone read as game code.
fn linked_sdk(metadata: &Value) -> Option<PathBuf> {
    let runtime = metadata["packages"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == "psx-rt" && p["source"].is_null())?;
    let manifest = Path::new(runtime["manifest_path"].as_str()?);
    manifest
        .ancestors()
        .skip(1)
        .find(|dir| dir.join("sdk").join("profile.toml").is_file())
        .map(Path::to_path_buf)
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
    let sdk_root = linked_sdk(metadata).unwrap_or_else(|| sdk_root.to_path_buf());
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
    // A bare name matches wherever it appears; `krate::name` matches only the item of that crate
    // (see `qualified_hits`).
    let (qualified, bare): (Vec<&String>, Vec<&String>) =
        profile.deprecated.iter().partition(|s| s.contains("::"));
    let qualified: Vec<(String, String)> = qualified
        .into_iter()
        .filter_map(|s| s.split_once("::"))
        .map(|(krate, name)| (krate.to_string(), name.to_string()))
        .collect();
    let deprecated = (!bare.is_empty()).then(|| {
        let names: Vec<String> = bare.iter().map(|s| regex::escape(s)).collect();
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
    let gpu_words = profile.gpu_words && !probe;
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
            let lines = code_lines(&text);
            let tables = mask_data_tables(&lines);
            let qualified_lines = qualified_hits(&lines, &qualified);
            for (n, code) in lines.iter().enumerate() {
                for (kind, pattern) in &line_rules {
                    let seen = if *kind == "register-literal" {
                        &tables[n]
                    } else {
                        code
                    };
                    if pattern.is_match(seen) {
                        hits.push(Hit {
                            kind,
                            item: item.clone(),
                            line: Some(n as u32 + 1),
                            text: code.trim().chars().take(100).collect(),
                        });
                    }
                }
                if gpu_words && gpu_word_in(code) {
                    hits.push(Hit {
                        kind: "gpu-word",
                        item: item.clone(),
                        line: Some(n as u32 + 1),
                        text: code.trim().chars().take(100).collect(),
                    });
                }
                // A line the bare-name rule already counted is not counted twice.
                if qualified_lines.contains(&n)
                    && !deprecated.as_ref().is_some_and(|p| p.is_match(code))
                {
                    hits.push(Hit {
                        kind: "deprecated-use",
                        item: item.clone(),
                        line: Some(n as u32 + 1),
                        text: code.trim().chars().take(100).collect(),
                    });
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
/// [--receipt EXE] [--gpu-words] [-- CARGO-ARGS]`:
/// run the gate and print the refusals. Exit 1 when there are any. With `--receipt EXE` a pass
/// also writes the gate receipt into the linked, patched PSX-EXE (what `psoxide-pgo apply` does
/// at the end of its build; a staged guest build, which does not go through `apply`, calls it).
pub fn main(args: &[String]) -> i32 {
    let mut sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (mut metadata, mut crate_dir, mut package) = (None, PathBuf::from("."), None);
    let mut cargo: Vec<String> = Vec::new();
    let mut propose = false;
    let mut program_file: Option<PathBuf> = None;
    let mut receipt: Option<PathBuf> = None;
    let mut gpu_words = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--sdk" => sdk = PathBuf::from(it.next().cloned().unwrap_or_default()),
            "--metadata" => metadata = it.next().map(PathBuf::from),
            "--crate" => crate_dir = PathBuf::from(it.next().cloned().unwrap_or_default()),
            "--package" => package = it.next().cloned(),
            "--propose" => propose = true,
            "--gpu-words" => gpu_words = true,
            "--program-file" => program_file = it.next().map(PathBuf::from),
            "--receipt" => receipt = it.next().map(PathBuf::from),
            "--" => {
                cargo.extend(it.by_ref().cloned());
            }
            other => {
                eprintln!("psx-gate: unknown argument {other}");
                return 2;
            }
        }
    }
    match run_with(
        &sdk,
        metadata.as_deref(),
        &crate_dir,
        package.as_deref(),
        &cargo,
        program_file.as_deref(),
        gpu_words,
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
                if let Some(exe) = &receipt {
                    if let Err(e) = write_receipt(exe, &outcome) {
                        eprintln!("psx-gate: {e}");
                        return 2;
                    }
                }
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

/// Write the receipt of `outcome` (a program that passed) into the PSX-EXE at `exe`.
fn write_receipt(exe: &Path, outcome: &Outcome) -> Result<(), String> {
    let mut bytes = fs::read(exe).map_err(|e| format!("cannot read {}: {e}", exe.display()))?;
    psx_gate::press::stamp(&mut bytes, &outcome.program, outcome.diag)
        .map_err(|_| format!("{} is not a PSX-EXE the receipt fits", exe.display()))?;
    fs::write(exe, bytes).map_err(|e| format!("cannot write {}: {e}", exe.display()))
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
    run_with(
        sdk,
        metadata,
        crate_dir,
        package,
        cargo_args,
        program_file,
        false,
    )
}

/// [`run`] with the optional `gpu-word` rule (`psx-gate --gpu-words`).
///
/// # Errors
///
/// As [`run`].
pub fn run_with(
    sdk: &Path,
    metadata: Option<&Path>,
    crate_dir: &Path,
    package: Option<&str>,
    cargo_args: &[String],
    program_file: Option<&Path>,
    gpu_words: bool,
) -> Result<Outcome, String> {
    let value: Value = match metadata {
        Some(file) => serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
        None => cargo_metadata(crate_dir, cargo_args)?,
    };
    // The SDK the program links, which may not be the one this tool was built from.
    let linked = linked_sdk(&value);
    let sdk = linked.as_deref().unwrap_or(sdk);
    let mut profile = Profile::read(&sdk.join("sdk").join("profile.toml"))?;
    profile.gpu_words = gpu_words;
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

    fn masked_register_hits(src: &str) -> usize {
        let lines = code_lines(src);
        mask_data_tables(&lines)
            .iter()
            .filter(|l| register_hit(l))
            .count()
    }

    /// A cloned game repository: its tools were built in another checkout, so the SDK root they
    /// are given names that checkout, not the SDK the clone links. The SDK crates are still SDK
    /// crates (psx-rt's register literal is not the program's), found by where psx-rt lives.
    #[test]
    fn the_sdk_is_the_one_the_program_links_not_the_tools_checkout() {
        let f = fixture("clone", NOTHING, &["runtime"], &[]);
        fs::write(f.dir.join("sdk-root/sdk/profile.toml"), "").unwrap();
        let elsewhere = f.dir.join("original-clone");
        fs::create_dir_all(&elsewhere).unwrap();
        let own = Profile::parse_program_file(NOTHING, Path::new("psx-gate.toml")).unwrap();
        let outcome = check(
            &f.metadata,
            None,
            None,
            &elsewhere,
            &profile(NOTHING),
            Some(&own),
        )
        .unwrap();
        assert!(outcome.violations.is_empty(), "{:?}", outcome.violations);
        assert_eq!(
            linked_sdk(&f.metadata).map(|p| p.canonicalize().unwrap()),
            Some(f.dir.join("sdk-root").canonicalize().unwrap())
        );
        // Without a profile above psx-rt nothing is found and the given root stands.
        fs::remove_file(f.dir.join("sdk-root/sdk/profile.toml")).unwrap();
        let outcome = check(
            &f.metadata,
            None,
            None,
            &elsewhere,
            &profile(NOTHING),
            Some(&own),
        )
        .unwrap();
        assert!(!outcome.violations.is_empty());
    }

    #[test]
    fn a_halfword_in_a_16_bit_table_is_data_not_a_register() {
        let table = "pub static PIX: [u16; 4] = [\n    0x0000, 0x1F80,\n    0xBF80, 0x9f80,\n];\nconst B: &[u8] = &[0x1f80 as u8];\n";
        assert_eq!(masked_register_hits(table), 0);
        // the data is in the table; the same literal beside it is still a register
        let beside = format!("{table}fn f() {{ let p = 0x1f80_1814 as *mut u32; }}\n");
        assert_eq!(masked_register_hits(&beside), 1);
    }

    #[test]
    fn a_register_in_a_32_bit_table_or_outside_a_table_is_still_refused() {
        // a u32 element holds a whole address
        assert_eq!(
            masked_register_hits("static REGS: [u32; 1] = [0x1F80_1814];\n"),
            1
        );
        // a scalar halfword is not a table
        assert_eq!(masked_register_hits("const HI: u16 = 0x1F80;\n"), 1);
        // the array length is code, not data
        assert_eq!(
            masked_register_hits("static T: [u16; 0x1F80] = [0; 4];\n"),
            1
        );
        // the mask ends at the item's `;`: code after the table is scanned
        assert_eq!(
            masked_register_hits("static T: [u16; 2] = [1, 2]; fn f() { g(0x1F80_1810) }\n"),
            1
        );
        // an unterminated initializer blanks nothing
        assert_eq!(masked_register_hits("static T: [u16; 2] = [0x1F80,\n"), 1);
    }

    #[test]
    fn a_16_bit_table_passes_the_gate_and_a_register_beside_it_does_not() {
        let table = "pub static PIX: [u16; 2] = [0x1F80, 0x0001];\n";
        let f = fixture("tab", table, &["runtime"], &[]);
        let out = run_check(&f, &profile(""), NOTHING);
        assert!(out.violations.is_empty(), "{:?}", out.violations);
        let f = fixture(
            "tab2",
            &format!("{table}fn f() {{ let p = 0x1f80_1814 as *mut u32; }}\n"),
            &["runtime"],
            &[],
        );
        let out = run_check(&f, &profile(""), NOTHING);
        assert_eq!(out.violations.len(), 1, "{:?}", out.violations);
        assert!(
            out.violations[0].contains("src/lib.rs:2"),
            "{:?}",
            out.violations
        );
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
    fn gpu_command_words_are_flagged_in_hex_decimal_and_shifted_forms() {
        for hit in [
            "let a = 0xE1000000;",
            "gp0(0xE100_0200 | mode);",
            "0xe3000000u32",
            "send(0xE6000003)",
            "0xE5_00_00_00",
            "gp1(0x0800_0001);",
            "gp1(0x0300_0000)",
            "gp1(0x0400_0002)",
            "gp1(0x0500_0000 | y << 10 | x)",
            "gp1(0x0200_0000)",
            "gp0(0x1F00_0000)",
            "let tri = 0x3000_0000;",
            "let cmd = (0xE1 << 24) | bits;",
            "let c = 0xA0u32 << 24;",
            "let c = 0xC0 << 24;",
            "let c = 225 << 24 | 5;",
            "gp0(3774873600)",
            "gp0(3_825_205_248u32)",
        ] {
            assert!(gpu_word_in(hit), "{hit}");
        }
    }

    #[test]
    fn hash_primes_flag_bits_bases_and_gte_words_are_not_gpu_words() {
        for pass in [
            "h = h.wrapping_mul(0x0100_0193);",
            "let basis = 0x811C_9DC5;",
            "let k = 0x9E37_79B9;",
            "let c = 0xE654_6B64;",
            "let c = 0x85EB_CA6B;",
            "let flag = 1 << 24;",
            "let busy = 1 << 28;",
            "let sign = 0x8000_0000;",
            "let kseg1 = 0xA000_0000;",
            "let bit = 0x4000_0000;",
            "let gte = 0x4A18_0001;",
            "let gte = 0x4B18_0001;",
            "let n = 16_777_619;",
            "let n = 1_000_000_000;",
            "let x = 0xE1;",
            "let x = 0xE100;",
            "let f = 3.0e10;",
            "let v = bytes[3] as u32) << 24;",
            "let w = (a as u32) << 24 | b;",
            "let w = 0x12 << 24;",
            "let w = 3 << 24;",
            "let one = 0x0100_0000;",
            "let id = 0x0000_E100;",
        ] {
            assert!(!gpu_word_in(pass), "{pass}");
        }
    }

    #[test]
    fn the_gpu_word_rule_is_off_unless_asked_and_a_probe_is_exempt() {
        let src = "fn f() {\n    gp0(0xE1000000);\n}\n";
        let f = fixture("gpuw", src, &["runtime"], &[]);
        let off = run_check(&f, &profile(""), NOTHING);
        assert!(off.violations.is_empty(), "{:?}", off.violations);
        let mut on = profile("");
        on.gpu_words = true;
        let out = run_check(&f, &on, NOTHING);
        assert_eq!(out.violations.len(), 1, "{:?}", out.violations);
        assert!(
            out.violations[0].contains("src/lib.rs:2") && out.violations[0].contains("gpu-word")
        );
        // A listed count passes it like the other counted kinds.
        let listed = exception("gpu-word", "src/lib.rs", Some(1));
        assert!(run_check(&f, &on, &listed).violations.is_empty());
        let mut probe = profile("[probe]\nprograms = [\"game\"]\n");
        probe.gpu_words = true;
        let inside = local_fixture("gpuw-probe", src, &["runtime"]);
        assert!(run_check(&inside, &probe, NOTHING).violations.is_empty());
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

    #[test]
    fn a_qualified_deprecated_symbol_matches_the_sdk_item_and_not_a_programs_own_method() {
        let with = profile("[deprecated]\nsymbols = [\"psx_pad::poll_two\"]\n");
        let own = "struct S;\nimpl S {\n    fn poll_two(&self) -> u8 { 0 }\n    fn f(&self) { let _ = self.poll_two(); }\n}\nfn g(s: &S) { s.poll_two(); }\n";
        let f = fixture("qual-own", own, &["runtime"], &[]);
        assert!(
            run_check(&f, &with, NOTHING).violations.is_empty(),
            "a program's own method passes"
        );
        let path = "fn f() { let _ = psx_pad::poll_two(); }\n";
        let f = fixture("qual-path", path, &["runtime"], &[]);
        let out = run_check(&f, &with, NOTHING);
        assert_eq!(out.violations.len(), 1, "{:?}", out.violations);
        assert!(out.violations[0].contains("deprecated-use"));
        let imported = "use psx_pad::{poll_two, Port};\nfn f() { let _ = poll_two(); }\n";
        let f = fixture("qual-use", imported, &["runtime"], &[]);
        let out = run_check(&f, &with, NOTHING);
        assert!(
            out.violations.iter().any(|v| v.contains("deprecated-use")),
            "{:?}",
            out.violations
        );
        let glob = "use psx_pad::*;\nfn f() { let _ = poll_two(); }\n";
        let f = fixture("qual-glob", glob, &["runtime"], &[]);
        assert!(!run_check(&f, &with, NOTHING).violations.is_empty());
        // Its own method next to an import of something else from the crate still passes.
        let both = "use psx_pad::Port;\nstruct S;\nimpl S { fn poll_two(&self) {} }\nfn f(s: &S) { s.poll_two(); }\n";
        let f = fixture("qual-other", both, &["runtime"], &[]);
        assert!(run_check(&f, &with, NOTHING).violations.is_empty());
    }
}
