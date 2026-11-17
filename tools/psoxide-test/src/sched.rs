//! The budget: run jobs on at most `width` threads, in the order given (callers sort longest
//! first), and return the results in that same order.

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
}
