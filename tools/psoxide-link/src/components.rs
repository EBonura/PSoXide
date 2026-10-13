//! Materialize locked source components at the paths Cargo expects.
//!
//! A game (or the editor, or the emulator) lists in `components.lock.json`
//! which paths it imports from which repository at which exact commit. This
//! exports those paths into the consumer's tree and records a receipt with
//! the SHA-256 of every imported file. Imported files are build inputs, not a
//! second maintained copy of the source, so the receipt is checked before
//! anything is reused or replaced: a local edit to an imported file stops the
//! refresh instead of being overwritten. Local sources export the locked
//! commit with `git archive`, never the working tree.
//!
//! A compound workspace's root Cargo.lock is resolved offline in a fresh
//! staging export before publication. This can add local path packages and
//! update their dependency edges, but never change a sourced package's locked
//! identity, checksum or dependency table. The resulting lock is sealed in
//! the same content receipt and subsequent local edits still stop refresh.
//!
//! This replaces `tools/bootstrap-components.py`. Given the same lock and
//! sources it writes byte-identical files, modes and receipt. Every write is a
//! new file, so imported files always carry a fresh mtime and a repin cannot
//! leave Cargo trusting an object built from the previous revision.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::sha256::sha256_hex;
use crate::Result;

/// The receipt beside the imported files.
pub const RECEIPT: &str = ".components-receipt.json";
/// The lock read when no `--lock` is given.
pub const LOCK: &str = "components.lock.json";

/// What a run did, for callers that report it themselves.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The receipt already matched the lock; nothing was written.
    Current,
    /// This many files were imported and the receipt rewritten.
    Bootstrapped(usize),
}

/// Normalize a component path the way `PurePosixPath` does and refuse one
/// that could leave the consumer's tree.
fn relative(value: &str) -> Result<String> {
    let parts: Vec<&str> = value
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if value.starts_with('/') || parts.is_empty() || parts.contains(&"..") {
        return Err(format!("Unsafe component path: {value}").into());
    }
    Ok(parts.join("/"))
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// Every imported file the receipt names must still hold the bytes it
/// recorded, or the user has local edits a refresh would destroy.
fn verify(root: &Path, receipt: &Value) -> Result<()> {
    if let Some(files) = receipt.get("files").and_then(Value::as_object) {
        for (name, expected) in files {
            let path = root.join(relative(name)?);
            let intact = path.is_file()
                && !path.is_symlink()
                && Some(sha256_hex(&fs::read(&path)?).as_str()) == expected.as_str();
            if !intact {
                return Err(format!(
                    "Imported file changed or missing: {}. Preserve your edits before refreshing components.",
                    path.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

/// The lock's component names in the order the file lists them.
/// `serde_json`'s map sorts its keys, and the lock order decides which
/// component reports an overlap and the order of the `Resolved` lines. The
/// lock has already parsed, so this only has to walk strings and nesting.
fn component_order(lock: &[u8]) -> Result<Vec<String>> {
    // One entry per open container: whether it is an object, and whether it
    // is the top-level "components" object.
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let mut names = Vec::new();
    let mut expect_key = false;
    let mut top_key: Option<String> = None;
    let mut i = 0;
    while i < lock.len() {
        match lock[i] {
            b'"' => {
                let start = i;
                i += 1;
                while lock[i] != b'"' {
                    i += if lock[i] == b'\\' { 2 } else { 1 };
                }
                let text: String = serde_json::from_slice(&lock[start..=i])?;
                if expect_key {
                    match stack.as_slice() {
                        [_] => top_key = Some(text),
                        [_, (true, true)] if !names.contains(&text) => names.push(text),
                        _ => {}
                    }
                }
                expect_key = false;
            }
            b'{' => {
                let components = stack.len() == 1 && top_key.as_deref() == Some("components");
                stack.push((true, components));
                expect_key = true;
            }
            b'[' => {
                stack.push((false, false));
                expect_key = false;
            }
            b'}' | b']' => {
                stack.pop();
                expect_key = false;
            }
            b',' => expect_key = stack.last().is_some_and(|&(object, _)| object),
            b':' => expect_key = false,
            _ => {}
        }
        i += 1;
    }
    Ok(names)
}

fn git(args: &[&str], dir: &Path) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            output.status
        )
        .into());
    }
    Ok(output.stdout)
}

/// `git archive` of the locked revision from a GitHub repository, fetched
/// into a throwaway bare repository. The bytes match the GitHub tarball the
/// Python tool downloaded: both are `git archive` of the same tree with the
/// default 002 umask.
fn fetch_remote(repository: &str, revision: &str) -> Result<Vec<u8>> {
    let scratch = std::env::temp_dir().join(format!(
        "psoxide-components-{}-{revision}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let result = (|| {
        git(&["init", "-q", "--bare"], &scratch)?;
        let url = format!("https://github.com/{repository}.git");
        git(
            &["fetch", "-q", "--depth", "1", "--no-tags", &url, revision],
            &scratch,
        )?;
        git(&["-c", "tar.umask=0002", "archive", revision], &scratch)
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

/// One regular file from an archive.
struct Entry {
    name: String,
    mode: u32,
    data: Vec<u8>,
}

fn octal(field: &[u8]) -> Result<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        // GNU base-256 for values that overflow the octal field.
        let mut value = u64::from(field[0] & 0x7f);
        for &b in &field[1..] {
            value = (value << 8) | u64::from(b);
        }
        return Ok(value);
    }
    let text: String = field
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    Ok(u64::from_str_radix(text, 8).map_err(|_| format!("bad tar number {text:?}"))?)
}

fn cstr(field: &[u8]) -> Result<String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    Ok(String::from_utf8(field[..end].to_vec()).map_err(|_| "non-UTF-8 tar member name")?)
}

/// A tar member as `tarfile` presents it: the name after pax and GNU long
/// names, a kind, a mode and its bytes.
enum Member {
    File(Entry),
    Directory(String),
    Other(String),
}

/// Read a POSIX (ustar or pax) or GNU tar stream, which is what `git archive`
/// writes.
fn members(archive: &[u8]) -> Result<Vec<Member>> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let size = usize::try_from(octal(&header[124..136])?)?;
        let start = offset + 512;
        let end = start
            .checked_add(size)
            .filter(|&end| end <= archive.len())
            .ok_or("truncated tar member")?;
        let data = &archive[start..end];
        offset = start + size.div_ceil(512) * 512;
        let kind = header[156];
        match kind {
            b'x' => {
                pax_path = pax_records(data)?.remove("path");
                continue;
            }
            b'g' => continue,
            b'L' => {
                long_name = Some(cstr(data)?);
                continue;
            }
            _ => {}
        }
        let mut name = cstr(&header[0..100])?;
        if &header[257..263] == b"ustar\0" {
            let prefix = cstr(&header[345..500])?;
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        if let Some(long) = long_name.take() {
            name = long;
        }
        if let Some(path) = pax_path.take() {
            name = path;
        }
        let directory = kind == b'5' || (kind == b'\0' && name.ends_with('/'));
        let mode = u32::try_from(octal(&header[100..108])?)? & 0o777;
        out.push(if directory {
            Member::Directory(name.trim_end_matches('/').to_string())
        } else if matches!(kind, b'0' | b'\0' | b'7') {
            Member::File(Entry {
                name,
                mode,
                data: data.to_vec(),
            })
        } else {
            Member::Other(name)
        });
    }
    Ok(out)
}

fn pax_records(mut data: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut records = BTreeMap::new();
    while !data.is_empty() && data[0] != 0 {
        let space = data
            .iter()
            .position(|&b| b == b' ')
            .ok_or("bad pax record")?;
        let length: usize = std::str::from_utf8(&data[..space])?.parse()?;
        let record = data.get(space + 1..length).ok_or("bad pax record")?;
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        let text = String::from_utf8(record.to_vec())?;
        if let Some((key, value)) = text.split_once('=') {
            records.insert(key.to_string(), value.to_string());
        }
        data = &data[length..];
    }
    Ok(records)
}

/// Python's `repr` of a sorted list of strings, for the error message.
fn python_list(items: &BTreeSet<String>) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|item| {
            let escaped = item.replace('\\', "\\\\");
            if item.contains('\'') && !item.contains('"') {
                format!("\"{escaped}\"")
            } else {
                format!("'{}'", escaped.replace('\'', "\\'"))
            }
        })
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// Python's `json.dumps(value, indent=2, sort_keys=True)` (with its default
/// `ensure_ascii`) plus the trailing newline the tool wrote. `serde_json`'s
/// map already keeps keys sorted.
fn python_json(value: &Value) -> String {
    fn string(out: &mut String, text: &str) {
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                ' '..='~' => out.push(c),
                _ => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
        }
        out.push('"');
    }
    fn write(out: &mut String, value: &Value, level: usize) {
        let pad = |out: &mut String, level: usize| {
            out.push('\n');
            out.push_str(&"  ".repeat(level));
        };
        match value {
            Value::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, level + 1);
                    write(out, item, level + 1);
                }
                pad(out, level);
                out.push(']');
            }
            Value::Object(map) if !map.is_empty() => {
                out.push('{');
                for (i, (key, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, level + 1);
                    string(out, key);
                    out.push_str(": ");
                    write(out, item, level + 1);
                }
                pad(out, level);
                out.push('}');
            }
            Value::Array(_) => out.push_str("[]"),
            Value::Object(_) => out.push_str("{}"),
            Value::String(text) => string(out, text),
            Value::Null => out.push_str("null"),
            Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::Number(number) => out.push_str(&number.to_string()),
        }
    }
    let mut out = String::new();
    write(&mut out, value, 0);
    out.push('\n');
    out
}

/// Write `data` to `path` through a temporary file in the same directory,
/// then give it `mode`. The rename replaces the inode, so the file's mtime is
/// always the time of this run.
fn replace(path: &Path, data: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path.parent().ok_or("component path has no parent")?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("component"),
        std::process::id()
    ));
    let mut file = fs::File::create(&temporary)?;
    file.write_all(data)?;
    drop(file);
    if let Some(mode) = mode {
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Cargo may add local packages when independently pinned components form a
/// new workspace. Existing sourced packages must retain their complete lock
/// tables, including checksums and resolved dependency identities.
fn validate_compound_lock(before: &[u8], after: &[u8]) -> Result<()> {
    let parse =
        |bytes: &[u8]| -> Result<toml::Value> { Ok(toml::from_str(std::str::from_utf8(bytes)?)?) };
    let original = parse(before)?;
    let resolved = parse(after)?;
    let mut original_header = original.clone();
    let mut resolved_header = resolved.clone();
    original_header
        .as_table_mut()
        .ok_or("invalid Cargo.lock header")?
        .remove("package");
    resolved_header
        .as_table_mut()
        .ok_or("invalid Cargo.lock header")?
        .remove("package");
    if original_header != resolved_header {
        return Err("compound resolution changed the lock format or metadata".into());
    }
    let packages = |value: &toml::Value| -> Result<Vec<toml::Value>> {
        Ok(value
            .get("package")
            .and_then(toml::Value::as_array)
            .ok_or("compound Cargo.lock has no package tables")?
            .clone())
    };
    let original_packages = packages(&original)?;
    let resolved_packages = packages(&resolved)?;
    let identity = |package: &toml::Value| {
        (
            package.get("name").cloned(),
            package.get("version").cloned(),
            package.get("source").cloned(),
        )
    };
    for package in &original_packages {
        let found = resolved_packages
            .iter()
            .find(|candidate| identity(candidate) == identity(package))
            .ok_or("compound resolution removed or changed a pinned package")?;
        if package.get("source").is_some() {
            if found != package {
                return Err("compound resolution changed a sourced package table".into());
            }
        } else {
            let mut old = package.clone();
            let mut new = found.clone();
            old.as_table_mut()
                .ok_or("invalid local package table")?
                .remove("dependencies");
            new.as_table_mut()
                .ok_or("invalid local package table")?
                .remove("dependencies");
            if old != new {
                return Err("compound resolution changed a local package identity".into());
            }
        }
    }
    for package in &resolved_packages {
        if package.get("source").is_some()
            && !original_packages
                .iter()
                .any(|old| identity(old) == identity(package))
        {
            return Err("compound resolution introduced an unpinned sourced package".into());
        }
    }
    Ok(())
}

/// Read the imported Rustup TOML or supported single-channel legacy file.
fn imported_toolchain(incoming: &BTreeMap<String, (Vec<u8>, u32)>) -> Result<Option<String>> {
    let modern = incoming.get("rust-toolchain.toml");
    let legacy = incoming.get("rust-toolchain");
    if modern.is_some() && legacy.is_some() {
        return Err("compound workspace has ambiguous toolchain files".into());
    }
    let Some((bytes, _)) = modern.or(legacy) else {
        return Ok(None);
    };
    let text = std::str::from_utf8(bytes)?.trim();
    let channel = if legacy.is_some() && !text.starts_with('[') {
        if text.is_empty() || text.chars().any(char::is_whitespace) {
            return Err("legacy toolchain file must contain one channel".into());
        }
        text.to_owned()
    } else {
        let value: toml::Value = toml::from_str(text)?;
        value
            .get("toolchain")
            .and_then(|table| table.get("channel"))
            .and_then(toml::Value::as_str)
            .filter(|channel| !channel.is_empty())
            .ok_or("compound workspace must name an installed toolchain channel")?
            .to_owned()
    };
    Ok(Some(channel))
}

/// Resolve a fresh compound workspace in an isolated export before any
/// consumer files or receipt are published. No existing hydration is edited.
fn resolve_compound_workspace(
    root: &Path,
    incoming: &mut BTreeMap<String, (Vec<u8>, u32)>,
) -> Result<()> {
    if !incoming.contains_key("Cargo.toml") || !incoming.contains_key("Cargo.lock") {
        return Ok(());
    }
    let toolchain = imported_toolchain(incoming)?;
    struct Stage(PathBuf);
    impl Drop for Stage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stage = root.join(format!(
        ".components-resolve-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&stage)?;
    let stage = Stage(stage);
    for (name, (bytes, mode)) in incoming.iter() {
        let path = stage.0.join(name);
        fs::create_dir_all(path.parent().ok_or("staged component has no parent")?)?;
        replace(&path, bytes, Some(*mode))?;
    }
    // Cargo-launched callers inherit a direct Cargo path and toolchain
    // override. Use the pinned channel explicitly; rustup run never installs
    // a missing channel unless --install is requested.
    let mut command = if let Some(channel) = toolchain {
        let mut command = Command::new("rustup");
        command.args(["run", &channel, "cargo"]);
        command.env_remove("RUSTUP_TOOLCHAIN");
        command
    } else {
        Command::new("cargo")
    };
    let output = command
        .current_dir(&stage.0)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .args([
            "metadata",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            "Cargo.toml",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("Fresh compound workspace resolution failed offline; no files or receipt were published: {}",
            String::from_utf8_lossy(&output.stderr)).into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let stage_root = stage.0.canonicalize()?;
    for package in metadata["packages"]
        .as_array()
        .ok_or("compound metadata has no packages")?
    {
        if !package["source"].is_null() {
            continue;
        }
        for path in package["manifest_path"].as_str().into_iter().chain(
            package["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|dependency| dependency["path"].as_str()),
        ) {
            if !Path::new(path).canonicalize()?.starts_with(&stage_root) {
                return Err(
                    "compound workspace references a path outside the pinned exports".into(),
                );
            }
        }
    }
    let resolved = fs::read(stage.0.join("Cargo.lock"))?;
    let (original, mode) = incoming
        .get("Cargo.lock")
        .ok_or("missing imported Cargo.lock")?;
    validate_compound_lock(original, &resolved)?;
    incoming.insert("Cargo.lock".into(), (resolved, *mode));
    Ok(())
}

/// Bring `root` to the lock: verify the previous receipt, export every
/// component path at its locked revision (from `sources[name]` when given,
/// else from GitHub), refuse to replace files the consumer owns, write the
/// files and the receipt, and delete imports the lock dropped. With `check`,
/// only report whether the receipt already matches.
pub fn materialize(
    root: &Path,
    sources: &BTreeMap<String, PathBuf>,
    check: bool,
    lock_path: Option<&Path>,
) -> Result<Outcome> {
    if !check {
        fs::create_dir_all(root)?;
    }
    let root = root.canonicalize()?;
    let lock_path = lock_path.map_or_else(|| root.join(LOCK), Path::to_path_buf);
    let lock_bytes = fs::read(&lock_path)?;
    let lock: Value = serde_json::from_slice(&lock_bytes)?;
    if lock.get("schema").and_then(Value::as_f64) != Some(1.0) {
        return Err("Unsupported component lock schema".into());
    }
    let receipt_path = root.join(RECEIPT);
    let previous = if receipt_path.exists() {
        read_json(&receipt_path)?
    } else {
        Value::Object(Default::default())
    };
    verify(&root, &previous)?;
    let lock_sha256 = sha256_hex(&lock_bytes);
    if previous.get("lock_sha256").and_then(Value::as_str) == Some(lock_sha256.as_str()) {
        println!("Components match the lock and content receipt");
        return Ok(Outcome::Current);
    }
    if check {
        return Err("Components are not bootstrapped at the locked revisions".into());
    }

    let mut incoming: BTreeMap<String, (Vec<u8>, u32)> = BTreeMap::new();
    for name in component_order(&lock_bytes)? {
        let spec = &lock["components"][&name];
        let revision = spec["revision"]
            .as_str()
            .ok_or("component revision must be a string")?;
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(format!("{name}: require a full Git commit").into());
        }
        let paths = spec["paths"]
            .as_array()
            .ok_or("component paths must be a list")?
            .iter()
            .map(|path| relative(path.as_str().ok_or("component path must be a string")?))
            .collect::<Result<Vec<_>>>()?;
        let archive = match sources.get(&name) {
            Some(source) => {
                let mut args = vec!["archive", revision, "--"];
                args.extend(paths.iter().map(String::as_str));
                git(&args, source)?
            }
            None => {
                let repository = spec["repository"].as_str().unwrap_or_default();
                let valid = repository.split_once('/').is_some_and(|(owner, repo)| {
                    [owner, repo].iter().all(|part| {
                        !part.is_empty()
                            && part
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                    })
                });
                if !valid {
                    return Err(format!("Invalid GitHub repository: {repository}").into());
                }
                fetch_remote(repository, revision)?
            }
        };
        let mut found = BTreeSet::new();
        let selected = |filename: &str, path: &str| {
            filename == path || filename.starts_with(&format!("{path}/"))
        };
        for member in members(&archive)? {
            let filename = match &member {
                Member::File(entry) => entry.name.clone(),
                Member::Directory(name) | Member::Other(name) => name.clone(),
            };
            if !paths.iter().any(|path| selected(&filename, path)) {
                continue;
            }
            let entry = match member {
                Member::Directory(_) => continue,
                Member::Other(_) => {
                    return Err(format!("Unsupported component entry: {filename}").into())
                }
                Member::File(entry) => entry,
            };
            let filename = relative(&filename)?;
            found.extend(
                paths
                    .iter()
                    .filter(|path| selected(&filename, path))
                    .cloned(),
            );
            if incoming.contains_key(&filename) {
                return Err(format!("Components overlap: {filename}").into());
            }
            incoming.insert(filename, (entry.data, entry.mode));
        }
        let missing: BTreeSet<String> = paths
            .iter()
            .filter(|p| !found.contains(*p))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{name}: locked source paths missing: {}",
                python_list(&missing)
            )
            .into());
        }
        println!("Resolved {name} at {revision}");
    }

    // Validate every collision before changing any files.
    let listed = git(&["ls-files", "-z"], &root)?;
    let tracked: BTreeSet<&[u8]> = listed.split(|&b| b == 0).collect();
    let previous_files = previous
        .get("files")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for name in incoming.keys() {
        let path = root.join(name);
        if tracked.contains(name.as_bytes())
            || (path.exists() && !previous_files.contains_key(name))
        {
            return Err(format!("Refusing to replace an owned file: {name}").into());
        }
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|d| d.starts_with(&root)) {
            if directory.is_symlink() {
                return Err(format!("Refusing a symlink parent: {name}").into());
            }
            parent = directory.parent();
        }
    }
    resolve_compound_workspace(&root, &mut incoming)?;
    let mut files = serde_json::Map::new();
    for (name, (data, mode)) in &incoming {
        let path = root.join(name);
        fs::create_dir_all(path.parent().ok_or("component path has no parent")?)?;
        replace(&path, data, Some(*mode))?;
        files.insert(name.clone(), Value::String(sha256_hex(data)));
    }
    for name in previous_files.keys() {
        if !incoming.contains_key(name) {
            fs::remove_file(root.join(name))?;
        }
    }
    let receipt = serde_json::json!({
        "schema": 1,
        "lock_sha256": lock_sha256,
        "components": lock["components"],
        "files": files,
    });
    replace(&receipt_path, python_json(&receipt).as_bytes(), None)?;
    println!(
        "Bootstrapped {} files; component receipt recorded",
        incoming.len()
    );
    Ok(Outcome::Bootstrapped(incoming.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_toolchain_parser_handles_toml_and_legacy_without_guessing() {
        let input = |name: &str, contents: &str| {
            BTreeMap::from([(name.to_owned(), (contents.as_bytes().to_vec(), 0o644))])
        };
        for (name, text) in [
            (
                "rust-toolchain.toml",
                "[toolchain]\nchannel = \"nightly-2026-03-25\"\nprofile = \"minimal\"\n",
            ),
            ("rust-toolchain", "nightly-2026-03-25\n"),
            (
                "rust-toolchain",
                "[toolchain]\nchannel = \"nightly-2026-03-25\"\n",
            ),
        ] {
            assert_eq!(
                imported_toolchain(&input(name, text)).unwrap().as_deref(),
                Some("nightly-2026-03-25")
            );
        }
        assert!(imported_toolchain(&input("rust-toolchain", "nightly\nextra")).is_err());
        assert!(imported_toolchain(&input("rust-toolchain.toml", "[toolchain")).is_err());
        assert!(imported_toolchain(&input(
            "rust-toolchain.toml",
            "[toolchain]\npath = \"/outside\"\n"
        ))
        .is_err());
        let mut both = input("rust-toolchain", "nightly");
        both.extend(input(
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"nightly\"\n",
        ));
        assert!(imported_toolchain(&both).is_err());
    }

    #[test]
    fn compound_resolution_rejects_sourced_drift_and_unpinned_additions() {
        let original = b"version = 4\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"abc\"\ndependencies = [\"dep\"]\n";
        assert!(validate_compound_lock(original, original).is_ok());
        let text = std::str::from_utf8(original).unwrap();
        for changed in [text.replace("1.0.0", "1.0.1"),
            text.replace("abc", "def"), text.replace("[\"dep\"]", "[\"other\"]"),
            format!("{text}\n[[package]]\nname = \"new\"\nversion = \"1.0.0\"\nsource = \"git+https://example.invalid/repo#123\"\n")] {
            assert!(validate_compound_lock(original, changed.as_bytes()).is_err());
        }
    }

    #[test]
    fn compound_resolution_only_changes_local_dependency_tables() {
        let original = b"version = 4\n[[package]]\nname = \"app\"\nversion = \"1.0.0\"\n";
        let resolved = b"version = 4\n[[package]]\nname = \"app\"\nversion = \"1.0.0\"\ndependencies = [\"local\"]\n[[package]]\nname = \"local\"\nversion = \"0.1.0\"\n";
        assert!(validate_compound_lock(original, resolved).is_ok());
        assert!(validate_compound_lock(
            original,
            b"version = 4\n[[package]]\nname = \"app\"\nversion = \"2.0.0\"\n"
        )
        .is_err());
        assert!(validate_compound_lock(
            original,
            b"version = 3\n[[package]]\nname = \"app\"\nversion = \"1.0.0\"\n"
        )
        .is_err());
    }

    #[test]
    fn paths_normalize_like_pure_posix_path() {
        assert_eq!(relative("sdk").unwrap(), "sdk");
        assert_eq!(
            relative("./tools//psoxide-link/").unwrap(),
            "tools/psoxide-link"
        );
        for unsafe_path in ["/etc", "", ".", "a/../b", ".."] {
            assert!(relative(unsafe_path).is_err(), "{unsafe_path:?}");
        }
    }

    #[test]
    fn components_keep_the_lock_order() {
        let lock = br#"{"schema":1,"components":{"sdk":{"a":1},"emulator":{"b":2},"editor":{}}}"#;
        assert_eq!(
            component_order(lock).unwrap(),
            ["sdk", "emulator", "editor"]
        );
        let nested = br#"{"list": ["components", {"components": {"no": 1}}], "components": {"z": {"paths": ["a", "b"], "x": {"y": 1}}, "a\"q": []}, "after": ["c", "d"], "schema": 1}"#;
        assert_eq!(component_order(nested).unwrap(), ["z", "a\"q"]);
    }

    #[test]
    fn receipt_json_matches_python_dumps() {
        let value = serde_json::json!({"b": [1, {}], "a": "caf\u{e9} \u{1F600}\u{7f}", "c": []});
        assert_eq!(
            python_json(&value),
            "{\n  \"a\": \"caf\\u00e9 \\ud83d\\ude00\\u007f\",\n  \"b\": [\n    1,\n    {}\n  ],\n  \"c\": []\n}\n"
        );
    }

    #[test]
    fn missing_paths_print_as_a_python_list() {
        let missing: BTreeSet<String> = ["b".to_string(), "a'".to_string()].into();
        assert_eq!(python_list(&missing), "[\"a'\", 'b']");
    }
}
