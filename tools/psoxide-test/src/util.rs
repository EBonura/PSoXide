//! Small shared pieces: digests, child processes, file globs, clocks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    psx_library::sha256_file(path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// The lowercase hex SHA-256 of bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A receipt key: SHA-256 over labelled parts in the order given.
#[derive(Default)]
pub struct Key {
    hasher: Sha256,
}

impl Key {
    /// A key for one stage of this harness version.
    pub fn new(stage: &str) -> Self {
        let mut key = Key::default();
        key.part("harness", env!("CARGO_PKG_VERSION"));
        key.part("stage", stage);
        key
    }

    /// Add one labelled part. Labels keep two different splits of the same text apart.
    pub fn part(&mut self, label: &str, value: &str) {
        for piece in [label, value] {
            self.hasher.update((piece.len() as u64).to_le_bytes());
            self.hasher.update(piece.as_bytes());
        }
    }

    /// Add a file by its digest, or `missing` when it does not exist.
    pub fn file(&mut self, label: &str, path: &Path) -> Result<(), String> {
        let digest = if path.is_file() {
            sha256_file(path)?
        } else {
            "missing".to_string()
        };
        self.part(label, &digest);
        Ok(())
    }

    /// The hex key.
    pub fn finish(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

/// Seconds since the epoch, for the timing sidecar.
pub fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The modification time of `path` in seconds since the epoch.
pub fn mtime_seconds(path: &Path) -> Option<f64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// How a child process ended.
pub struct Finished {
    /// Exit code (`-1` when killed by a signal or by the timeout).
    pub code: i32,
    /// The whole of stdout and stderr, as written to the log.
    pub output: String,
    /// The timeout fired and the child was killed.
    pub timed_out: bool,
}

/// Run `argv` in `dir` with stdout and stderr to `log`, killing it after `timeout_s` seconds.
/// `env` adds or (with `None`) removes variables. Never prints the environment.
pub fn run_logged(
    argv: &[String],
    dir: &Path,
    env: &[(String, Option<String>)],
    log: &Path,
    timeout_s: Option<u64>,
) -> Result<Finished, String> {
    run_watched(argv, dir, env, log, timeout_s, None)
}

/// `run_logged`, also killing the child as soon as its log contains `stop_when` (an emulator
/// that pauses itself on a fault stays alive and silent until its timeout otherwise).
pub fn run_watched(
    argv: &[String],
    dir: &Path,
    env: &[(String, Option<String>)],
    log: &Path,
    timeout_s: Option<u64>,
    stop_when: Option<&str>,
) -> Result<Finished, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("empty command".into());
    };
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let file = fs::File::create(log).map_err(|e| format!("create {}: {e}", log.display()))?;
    let err = file
        .try_clone()
        .map_err(|e| format!("log {}: {e}", log.display()))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err);
    for (name, value) in env {
        match value {
            Some(v) => command.env(name, v),
            None => command.env_remove(name),
        };
    }
    // Its own process group, killed when `group` drops: nothing it starts outlives this call.
    let (mut child, group) = crate::procs::spawn(&mut command)
        .map_err(|e| format!("start `{program}` in {}: {e}", dir.display()))?;
    let start = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("wait for `{program}`: {e}"))?
        {
            break status;
        }
        let polls = start.elapsed().as_millis() / 50;
        let stop = stop_when.is_some_and(|needle| {
            polls.is_multiple_of(20) && fs::read_to_string(log).is_ok_and(|t| t.contains(needle))
        });
        if stop {
            crate::procs::kill(&group);
            break child
                .wait()
                .map_err(|e| format!("wait for `{program}`: {e}"))?;
        }
        if timeout_s.is_some_and(|t| start.elapsed().as_secs() >= t) {
            timed_out = true;
            // Our own child's process group only.
            crate::procs::kill(&group);
            break child
                .wait()
                .map_err(|e| format!("wait for `{program}`: {e}"))?;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // Grandchildren the child left running die with the group here.
    drop(group);
    let output = fs::read_to_string(log).unwrap_or_default();
    Ok(Finished {
        code: status.code().unwrap_or(-1),
        output,
        timed_out,
    })
}

/// Files matching `pattern` relative to `root`. Only the last path component may hold one `*`;
/// the result is sorted, so the order never depends on the file system.
pub fn glob(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let (dir, name) = match pattern.rsplit_once('/') {
        Some((dir, name)) => (root.join(dir), name),
        None => (root.to_path_buf(), pattern),
    };
    if pattern[..pattern.len() - name.len()].contains('*') {
        return Err(format!(
            "`{pattern}`: only the last path component may contain `*`"
        ));
    }
    let Some((prefix, suffix)) = name.split_once('*') else {
        let path = dir.join(name);
        return Ok(if path.exists() {
            vec![path]
        } else {
            Vec::new()
        });
    };
    if suffix.contains('*') {
        return Err(format!("`{pattern}`: at most one `*`"));
    }
    let mut out = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.len() >= prefix.len() + suffix.len()
            && file_name.starts_with(prefix)
            && file_name.ends_with(suffix)
            && entry.path().is_file()
        {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// Short form of a hex digest for tables.
/// `path` relative to `root` when it lies inside it, else as it is.
pub fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

/// Read a git value in `repo`.
pub fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Every file git would hand to a fresh clone plus untracked, not-ignored files, sorted.
pub fn tree_files(repo: &Path) -> Result<Vec<String>, String> {
    let listing = git(
        repo,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    let mut files: Vec<String> = listing
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_parts_are_length_delimited() {
        let mut a = Key::new("s");
        a.part("x", "ab");
        a.part("y", "c");
        let mut b = Key::new("s");
        b.part("x", "a");
        b.part("y", "bc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn glob_matches_one_star_in_the_last_component() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("t")).unwrap();
        for name in ["b.toml", "a.toml", "c.txt"] {
            fs::write(dir.path().join("t").join(name), "").unwrap();
        }
        let hits = glob(dir.path(), "t/*.toml").unwrap();
        let names: Vec<_> = hits
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.toml", "b.toml"]);
        assert!(glob(dir.path(), "*/x.toml").is_err());
        assert!(glob(dir.path(), "t/nothing*").unwrap().is_empty());
    }

    #[test]
    fn a_command_past_its_timeout_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let argv = ["sleep".to_string(), "5".to_string()];
        let done = run_logged(&argv, dir.path(), &[], &log, Some(1)).unwrap();
        assert!(done.timed_out);
    }
}
