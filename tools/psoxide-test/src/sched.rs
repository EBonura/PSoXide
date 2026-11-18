//! The budget: run jobs on at most `width` threads, in the order given (callers sort longest
//! first), and return the results in that same order.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Mutex;

/// Run `work` on every job, at most `width` at once. Results come back in job order.
pub fn run_all<J: Sync, R: Send>(
    jobs: &[J],
    width: usize,
    work: impl Fn(&J) -> R + Sync,
) -> Vec<R> {
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..jobs.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..width.max(1).min(jobs.len().max(1)) {
            scope.spawn(|| loop {
                let index = {
                    let mut n = next.lock().expect("job counter lock");
                    let i = *n;
                    *n += 1;
                    i
                };
                let Some(job) = jobs.get(index) else { break };
                let result = work(job);
                results.lock().expect("results lock")[index] = Some(result);
            });
        }
    });
    results
        .into_inner()
        .expect("results lock")
        .into_iter()
        .map(|r| r.expect("every job ran"))
        .collect()
}

/// Hold one of `count` machine-wide slots named `kind` in `dir` while `work` runs: an exclusive
/// `flock` on `<dir>/<kind>-<n>.lock`, released when the file closes (also when the process dies).
/// With no shared directory configured the work runs at once.
pub fn machine_slot<R>(
    dir: Option<&Path>,
    kind: &str,
    count: usize,
    work: impl FnOnce() -> R,
) -> R {
    let Some(dir) = dir else {
        return work();
    };
    if std::fs::create_dir_all(dir).is_err() {
        return work();
    }
    loop {
        for n in 0..count.max(1) {
            let path = dir.join(format!("{kind}-{n}.lock"));
            let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
            else {
                continue;
            };
            // SAFETY: a valid open descriptor owned by `file`, which outlives the lock: the lock
            // is released when `file` drops at the end of this scope.
            let locked =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
            if locked {
                let r = work();
                drop(file);
                return r;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Hold `n` of the `count` machine-wide slots `kind` at once while `work` runs (a job that
/// starts emulators of its own). All or nothing: a partial set is let go before waiting again,
/// so two jobs each wanting several slots cannot hold each other up.
pub fn machine_slots<R>(
    dir: Option<&Path>,
    kind: &str,
    count: usize,
    n: usize,
    work: impl FnOnce() -> R,
) -> R {
    let Some(dir) = dir else {
        return work();
    };
    let n = n.clamp(1, count.max(1));
    if std::fs::create_dir_all(dir).is_err() {
        return work();
    }
    loop {
        let mut held = Vec::new();
        for i in 0..count.max(1) {
            if held.len() == n {
                break;
            }
            let path = dir.join(format!("{kind}-{i}.lock"));
            let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
            else {
                continue;
            };
            // SAFETY: a valid open descriptor owned by `file`, kept in `held` while locked.
            let locked =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
            if locked {
                held.push(file);
            }
        }
        if held.len() == n {
            let r = work();
            drop(held);
            return r;
        }
        drop(held);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn results_keep_job_order_and_width_is_respected() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let jobs: Vec<u64> = (0..12).collect();
        let out = run_all(&jobs, 3, |j| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(5 * (12 - j)));
            running.fetch_sub(1, Ordering::SeqCst);
            j * 2
        });
        assert_eq!(out, (0..12).map(|j| j * 2).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn machine_slots_bound_work_across_threads() {
        let dir = tempfile::tempdir().unwrap();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let jobs: Vec<u32> = (0..6).collect();
        run_all(&jobs, 6, |_| {
            machine_slot(Some(dir.path()), "emu", 2, || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(30));
                running.fetch_sub(1, Ordering::SeqCst);
            })
        });
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(machine_slot(None, "emu", 1, || 7), 7);
    }

    #[test]
    fn several_slots_are_taken_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let in_use = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let jobs: Vec<u32> = (0..4).collect();
        run_all(&jobs, 4, |_| {
            machine_slots(Some(dir.path()), "emu", 3, 2, || {
                let now = in_use.fetch_add(2, Ordering::SeqCst) + 2;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(30));
                in_use.fetch_sub(2, Ordering::SeqCst);
            })
        });
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "never more than 3 slots in use"
        );
    }
}
