//! `material-audit`: flag firmware artifacts and vendor text in homebrew EXE
//! headers.
//!
//! The default scans tracked working files (plus untracked, nonignored
//! files). `--history` scans objects reachable from fetched remote refs and
//! tags. This is a heuristic inventory, not a proof of source authorship.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;

const VENDOR: &[u8] = b"sony computer entertainment";
const EXE_HEADER: u64 = 4 * 1024 * 1024;

fn artifact() -> Regex {
    Regex::new(
        r"(?i)(?:^|/)(?:bios|scph|sce[aei]?)[^/]*\.(?:bin|rom|img|png|jpg)$|bios.*\.(?:png|jpg)$|(?:^|/)psyq(?:/|\.)",
    )
    .expect("valid regex")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Why `path` (with `size` bytes, starting with `header`) needs a look.
pub fn reasons(path: &str, size: u64, header: &[u8]) -> Vec<&'static str> {
    let mut found = Vec::new();
    if artifact().is_match(path) {
        found.push("firmware/SDK artifact path");
    }
    if size == 524_288 {
        found.push("512 KiB blob; inspect its provenance");
    }
    let exe = header.starts_with(b"PS-X EXE");
    let lower = header.to_ascii_lowercase();
    let vendor_area = &lower[0x4c.min(lower.len())..0x800.min(lower.len())];
    if exe && contains(vendor_area, VENDOR) {
        found.push("vendor text in homebrew executable header");
    }
    if exe && contains(&lower, b"sony bios") {
        found.push("vendor firmware prompt in homebrew executable");
    }
    found
}

fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input));
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("git {} returned {}", args.join(" "), output.status));
    }
    Ok(output.stdout)
}

/// One scanned file or blob: name, size and its leading bytes.
pub type Entry = (String, u64, Vec<u8>);

fn header_len(name: &str) -> u64 {
    if name.to_lowercase().ends_with(".exe") {
        EXE_HEADER
    } else {
        2048
    }
}

/// Tracked plus untracked, nonignored files in the working tree.
pub fn scan_working(repo: &Path) -> Result<Vec<Entry>, String> {
    let listed = git(
        repo,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        None,
    )?;
    let mut names: Vec<&[u8]> = listed
        .split(|&b| b == 0)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let mut entries = Vec::new();
    for raw in names {
        let name = String::from_utf8_lossy(raw).into_owned();
        let path = repo.join(&name);
        if path.is_symlink() || !path.is_file() {
            continue;
        }
        let mut header = Vec::new();
        File::open(&path)
            .and_then(|f| f.take(header_len(&name)).read_to_end(&mut header))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let size = path.metadata().map_err(|e| e.to_string())?.len();
        entries.push((name, size, header));
    }
    Ok(entries)
}

/// Every blob reachable from fetched remote refs and tags.
pub fn scan_history(repo: &Path) -> Result<Vec<Entry>, String> {
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes",
            "refs/tags",
        ],
        None,
    )?;
    if refs.iter().all(|b| b.is_ascii_whitespace()) {
        return Err("no fetched remote refs or tags; refusing an empty history audit".into());
    }
    let objects = git(repo, &["rev-list", "--objects", "--stdin"], Some(&refs))?;
    let metadata = git(
        repo,
        &[
            "cat-file",
            "--batch-check=%(objecttype) %(objectname) %(objectsize) %(rest)",
        ],
        Some(&objects),
    )?;
    let mut entries = Vec::new();
    for line in metadata.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let mut fields = line.splitn(4, |&b| b == b' ');
        let kind = fields.next().unwrap_or_default();
        let oid = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        let size: u64 = String::from_utf8_lossy(fields.next().unwrap_or_default())
            .parse()
            .map_err(|_| format!("bad object size for {oid}"))?;
        if kind != b"blob" {
            continue;
        }
        let name = fields.next().map_or_else(
            || oid.clone(),
            |rest| String::from_utf8_lossy(rest).into_owned(),
        );
        let mut header = Vec::new();
        if name.to_lowercase().ends_with(".exe") {
            // Inspect executable headers with a bounded read.
            let mut child = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["cat-file", "blob", &oid])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            let stdout = child.stdout.take().expect("piped stdout");
            stdout
                .take(EXE_HEADER)
                .read_to_end(&mut header)
                .map_err(|e| e.to_string())?;
            // Closing the pipe early may end git with SIGPIPE, which is fine.
            let status = child.wait().map_err(|e| e.to_string())?;
            let piped = std::os::unix::process::ExitStatusExt::signal(&status) == Some(13)
                || status.code() == Some(141);
            if !status.success() && !piped {
                return Err(format!("cannot inspect object {oid}"));
            }
        }
        entries.push((name, size, header));
    }
    Ok(entries)
}

/// Exit 0 when clean, 1 with findings, 2 when the audit could not run.
pub fn main(args: &[String]) -> i32 {
    let mut repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut history = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--history" => history = true,
            "--repo" => match args.next() {
                Some(path) => repo = PathBuf::from(path),
                None => {
                    eprintln!("--repo needs a path");
                    return 2;
                }
            },
            other => match other.strip_prefix("--repo=") {
                Some(path) => repo = PathBuf::from(path),
                None => {
                    eprintln!("usage: material-audit [--repo PATH] [--history]");
                    return 2;
                }
            },
        }
    }
    let repo = repo.canonicalize().unwrap_or(repo);
    let scanned = if history {
        scan_history(&repo)
    } else {
        scan_working(&repo)
    };
    let entries = match scanned {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("Audit failed: {error}");
            return 2;
        }
    };
    let mut failures = 0;
    for (name, size, header) in &entries {
        for reason in reasons(name, *size, header) {
            println!("FINDING: {name}: {reason}");
            failures += 1;
        }
    }
    println!(
        "Scanned {} files/blobs; {failures} heuristic findings. Source provenance still requires review.",
        entries.len()
    );
    i32::from(failures > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercase_path_and_renamed_blob() {
        assert!(!reasons("assets/My BIOS.BIN", 524_288, b"").is_empty());
        assert!(!reasons("opaque.data", 524_288, b"").is_empty());
        assert!(!reasons("bios/SCPH1001.BIN", 1, b"").is_empty());
    }

    #[test]
    fn vendor_marker_in_executable_header() {
        let mut header = vec![0u8; 2048];
        header[..8].copy_from_slice(b"PS-X EXE");
        let marker = b"Sony Computer Entertainment Inc.";
        header[0x4c..0x4c + marker.len()].copy_from_slice(marker);
        assert!(!reasons("demo.exe", 800_000, &header).is_empty());
        header[0x4c..].fill(0);
        assert!(reasons("demo.exe", 800_000, &header).is_empty());
        assert!(reasons("sdk/target-mipsel-sony-psx.txt", 128, marker).is_empty());
    }

    #[test]
    fn history_finds_deleted_tagged_artifact() {
        let repo = std::env::temp_dir().join(format!("xtask-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Audit fixture"]);
        run(&["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(repo.join("SCPH1001.BIN"), vec![0u8; 524_288]).unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "fixture"]);
        run(&["tag", "published-fixture"]);
        run(&["rm", "-q", "SCPH1001.BIN"]);
        run(&["commit", "-qm", "remove fixture"]);
        assert!(scan_working(&repo).unwrap().is_empty());
        assert!(scan_history(&repo)
            .unwrap()
            .iter()
            .any(|(name, size, header)| !reasons(name, *size, header).is_empty()));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn missing_repository_fails() {
        assert_eq!(main(&["--repo".into(), "/does-not-exist".into()]), 2);
    }
}
