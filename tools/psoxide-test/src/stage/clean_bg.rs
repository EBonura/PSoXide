//! The full tier's `clean` stage, overlapped: it needs only the judged build's output and the
//! pushed commit, and no stage reads its result, so it can start as soon as `build` passes and
//! run beside `size`, `host`, `speed`, `journeys` and `cross`.
//!
//! It runs in its own process (`psoxide-test clean-stage`, the same code `run` calls inline), in
//! its own process group with its own reaper, so cancelling it reaches every cargo and git it
//! started. It holds one `jobs` unit of the parent's budget while it runs (the host checks run
//! one group fewer at a time) and no emulator slot. The parent joins it where `clean` sits in
//! the run order, so the result's stages, checks and verdict are what a serial run gives; only
//! the timing sidecar shows the overlap. A stage that stops the run first cancels it.
//!
//! It stays serial when `[budget] jobs` is 1 or the work tree's disk has less free space than
//! `[budget] overlap_min_free_gb` (the clean clone is a whole tree and its build).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::report::{Outcome, Source, StageResult, Timing};
use crate::stage::Ctx;

/// What the clean process hands back: its stage result and its timings.
#[derive(Debug, Serialize, Deserialize)]
pub struct Handback {
    pub stage: StageResult,
    pub timing: Vec<Timing>,
}

/// Why the clean stage must run serially, or `None` when it may overlap.
pub fn serial_reason(jobs: usize, free_bytes: Option<u64>, floor_gb: u64) -> Option<String> {
    if jobs <= 1 {
        return Some("[budget] jobs is 1".into());
    }
    let floor = floor_gb.saturating_mul(1 << 30);
    match free_bytes {
        None => Some("the work tree's free disk space cannot be read".into()),
        Some(free) if free < floor => Some(format!(
            "{:.1} GB free on the work tree's disk, under the {floor_gb} GB floor \
             ([budget] overlap_min_free_gb)",
            free as f64 / f64::from(1u32 << 30)
        )),
        Some(_) => None,
    }
}

/// Free bytes on the filesystem that holds `path` (what an unprivileged writer may use).
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain data; all zeroes is a valid value of it.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `s` a valid out-parameter for the call.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &raw mut s) };
    (rc == 0).then(|| u64::from(s.f_bavail).saturating_mul(s.f_frsize as u64))
}

/// A clean stage running in its own process.
pub struct Background {
    child: Option<Child>,
    _group: crate::procs::Group,
    out: PathBuf,
    log: PathBuf,
}

/// Start the clean process for the build `ctx` just judged. The caller has checked
/// [`serial_reason`].
pub fn start(ctx: &Ctx<'_>) -> Result<Background, String> {
    let dir = ctx.dir(&["clean-bg"])?;
    let artifacts = dir.join("artifacts.json");
    let out = dir.join("handback.json");
    let log = dir.join("clean.log");
    let _ = std::fs::remove_file(&out);
    std::fs::write(
        &artifacts,
        serde_json::to_string(&ctx.artifacts).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", artifacts.display()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = std::fs::File::create(&log).map_err(|e| format!("create {}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .args(["clean-stage", "--repo"])
        .arg(&ctx.repo)
        .arg("--artifacts")
        .arg(&artifacts)
        .arg("--out")
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err);
    let (child, group) =
        crate::procs::spawn(&mut command).map_err(|e| format!("start the clean process: {e}"))?;
    Ok(Background {
        child: Some(child),
        _group: group,
        out,
        log,
    })
}

impl Background {
    /// Wait for it and take its result (its output is printed here, where a serial run would
    /// have printed it).
    pub fn join(mut self) -> (StageResult, Vec<Timing>) {
        let status = self.child.take().map(|mut c| c.wait());
        if let Ok(text) = std::fs::read_to_string(&self.log) {
            print!("{text}");
        }
        let failed = |why: String| {
            (
                StageResult {
                    name: "clean".into(),
                    source: Source::Run,
                    key: String::new(),
                    outcome: Outcome::error("clean", why),
                },
                Vec::new(),
            )
        };
        match std::fs::read_to_string(&self.out) {
            Ok(text) => match serde_json::from_str::<Handback>(&text) {
                Ok(h) => (h.stage, h.timing),
                Err(e) => failed(format!("{}: {e}", self.out.display())),
            },
            Err(_) => failed(format!(
                "the clean process ({}) wrote no result; log {}",
                match status {
                    Some(Ok(s)) => s.to_string(),
                    Some(Err(e)) => e.to_string(),
                    None => "not started".into(),
                },
                self.log.display()
            )),
        }
    }

    /// Stop it: SIGTERM lets its signal handler kill every group it started, then the group
    /// is killed and reaped when this is dropped.
    pub fn cancel(mut self) {
        if let Some(mut child) = self.child.take() {
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: a signal to our own unreaped child.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            let _ = child.wait();
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: as in `cancel`.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            let _ = child.wait();
        }
    }
}

/// `psoxide-test clean-stage`: the clean stage alone, for the build whose artifacts the
/// parent recorded; writes the [`Handback`] to `out`.
pub fn child(ctx: &mut Ctx<'_>, artifacts: &Path, out: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(artifacts)
        .map_err(|e| format!("read {}: {e}", artifacts.display()))?;
    ctx.artifacts = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let stage = ctx.timed("clean", || super::clean::run(ctx));
    let timing = ctx.timing.lock().map(|t| t.clone()).unwrap_or_default();
    let body = serde_json::to_string(&Handback { stage, timing }).map_err(|e| e.to_string())?;
    let tmp = out.with_extension("tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("write {}: {e}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Check;

    #[test]
    fn it_stays_serial_on_one_job_or_low_disk() {
        let gb = 1u64 << 30;
        assert!(serial_reason(1, Some(100 * gb), 9)
            .unwrap()
            .contains("jobs is 1"));
        let low = serial_reason(3, Some(8 * gb), 9).unwrap();
        assert!(
            low.contains("8.0 GB free") && low.contains("9 GB floor"),
            "{low}"
        );
        assert!(serial_reason(3, None, 9).is_some());
        assert_eq!(serial_reason(3, Some(9 * gb), 9), None);
        assert_eq!(serial_reason(2, Some(40 * gb), 9), None);
    }

    #[test]
    fn free_space_is_read_from_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(dir.path()).is_some_and(|b| b > 0));
        assert_eq!(free_bytes(&dir.path().join("no/such/dir")), None);
    }

    #[test]
    fn a_handback_round_trips_the_stage_result_exactly() {
        let stage = StageResult {
            name: "clean".into(),
            source: Source::Run,
            key: "abc".into(),
            outcome: Outcome {
                checks: vec![Check::pass("ship clean clone", "2 files byte-identical")],
                ..Outcome::default()
            },
        };
        let h = Handback {
            stage: stage.clone(),
            timing: vec![Timing {
                what: "clean".into(),
                start: 1.0,
                end: 2.0,
            }],
        };
        let back: Handback = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        // The result file serializes stages: the joined stage must be the same bytes.
        assert_eq!(
            serde_json::to_string(&back.stage).unwrap(),
            serde_json::to_string(&stage).unwrap()
        );
        assert_eq!(back.stage.name, "clean");
        assert_eq!(back.timing.len(), 1);
    }

    #[test]
    fn a_process_that_writes_nothing_is_a_clean_failure_naming_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("clean.log");
        std::fs::write(&log, "fetch failed\n").unwrap();
        let mut command = Command::new("true");
        let (child, group) = crate::procs::spawn(&mut command).unwrap();
        let bg = Background {
            child: Some(child),
            _group: group,
            out: dir.path().join("handback.json"),
            log: log.clone(),
        };
        let (stage, timing) = bg.join();
        assert_eq!(stage.name, "clean");
        assert!(!stage.passed());
        assert!(timing.is_empty());
        assert!(stage.outcome.checks[0]
            .detail
            .contains(&log.display().to_string()));
    }

    #[test]
    fn cancel_stops_a_running_process() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let (child, group) = crate::procs::spawn(&mut command).unwrap();
        let pid = child.id();
        let dir = tempfile::tempdir().unwrap();
        let bg = Background {
            child: Some(child),
            _group: group,
            out: dir.path().join("h.json"),
            log: dir.path().join("l.log"),
        };
        let t = std::time::Instant::now();
        bg.cancel();
        assert!(t.elapsed().as_secs() < 10, "cancel waited for the sleep");
        // SAFETY: probing only; the process has been reaped.
        let alive = unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) } == 0;
        assert!(!alive);
    }
}
