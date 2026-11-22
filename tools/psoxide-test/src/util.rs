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

/// The lowercase hex SHA-256 of `path` as committed at `commit` in `repo` (the blob's raw bytes).
pub fn git_blob_sha256(repo: &Path, commit: &str, path: &str) -> Result<String, String> {
    let spec = format!("{commit}:{path}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "blob", &spec])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git cat-file blob {spec} in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(sha256_bytes(&output.stdout))
}

/// `seconds` since the Unix epoch as an RFC 3339 UTC time, `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339_utc(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rest = seconds % 86_400;
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The inverse of [`rfc3339_utc`]: seconds since the Unix epoch of `YYYY-MM-DDTHH:MM:SSZ`, or
/// `None` for any other shape (an offset other than `Z`, fractions, a date that does not exist).
pub fn parse_rfc3339_utc(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let shape_ok = b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape_ok {
        return None;
    }
    let num = |from: usize, to: usize| text[from..to].parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || day < 1 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days since 1970-01-01 from the civil date (proleptic Gregorian).
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()?;
    // A day past the month's end (02-30) maps to another date: refuse it by the round trip.
    (rfc3339_utc(seconds) == text).then_some(seconds)
}

/// Now, as [`rfc3339_utc`].
pub fn utc_now() -> String {
    rfc3339_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
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

/// `git -C repo args...` fed `input` on stdin, with `env` set, returning its raw stdout.
pub fn git_stdin(
    repo: &Path,
    args: &[&str],
    input: &[u8],
    env: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    // Written from a thread: a large output fills the pipe before the input is all read.
    let mut pipe = child.stdin.take().ok_or("git has no stdin")?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || pipe.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let written = writer.join().map_err(|_| "git stdin writer panicked")?;
    if !output.status.success() {
        return Err(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    written.map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    Ok(output.stdout)
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
    fn utc_times_are_rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_795_350_896), "2026-11-22T12:34:56Z");
        for t in [0, 951_782_400, 1_795_350_896, 4_102_444_799] {
            assert_eq!(parse_rfc3339_utc(&rfc3339_utc(t)), Some(t));
        }
        for bad in [
            "2026-11-22T12:34:56+00:00",
            "2026-11-22 12:34:56Z",
            "2026-02-30T00:00:00Z",
            "2026-11-22T24:00:00Z",
            "2026-11-22T12:34:56.5Z",
            "",
        ] {
            assert_eq!(parse_rfc3339_utc(bad), None, "{bad}");
        }
    }

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
