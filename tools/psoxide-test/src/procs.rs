//! Child process ownership: nothing the harness starts outlives it.
//!
//! Every child leads its own process group, so one `killpg` reaches everything it started (make,
//! cargo and rustc; the gate; Redux). Three layers stop a group:
//! - [`Group`] kills its group when dropped: on a normal return (leftover grandchildren), on an
//!   early error return and on a panic.
//! - [`install_signal_handlers`]: SIGINT, SIGTERM and SIGHUP kill every live group and exit.
//! - [`start_reaper`]: a small `psoxide-test reap` process in its own group reads the live group
//!   list from a pipe and kills whatever is left when the pipe closes, which also covers a harness
//!   killed by SIGKILL, where no handler runs.
//!
//! The machine-wide budget slots are `flock`s on close-on-exec descriptors of the harness, so the
//! kernel releases them when the harness process ends, however it ends.

use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

/// Live process groups, readable from a signal handler (no locks there).
static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

/// The pipe to the reaper process, when one runs.
static REAPER: Mutex<Option<(Child, ChildStdin)>> = Mutex::new(None);

/// Spawn `command` as the leader of a new process group and track that group.
pub fn spawn(command: &mut Command) -> std::io::Result<(Child, Group)> {
    command.process_group(0);
    let child = command.spawn()?;
    // `process_group(0)` makes the group id the child's pid.
    let pgid = i32::try_from(child.id()).unwrap_or(0);
    Ok((child, Group::track(pgid)))
}

/// A tracked process group; dropping it kills the whole group.
pub struct Group {
    pgid: i32,
}

impl Group {
    fn track(pgid: i32) -> Self {
        if pgid > 0 {
            if let Some(slot) = LIVE.iter().find(|s| {
                s.compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            }) {
                debug_assert_eq!(slot.load(Ordering::SeqCst), pgid);
            }
            tell_reaper(&format!("+{pgid}"));
        }
        Group { pgid }
    }

    /// The process group id (the leader's pid).
    #[cfg(test)]
    pub fn id(&self) -> i32 {
        self.pgid
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.pgid <= 0 {
            return;
        }
        kill_group(self.pgid);
        for slot in &LIVE {
            let _ = slot.compare_exchange(self.pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
        tell_reaper(&format!("-{}", self.pgid));
    }
}

/// SIGKILL every process in `group` now (it stays tracked until dropped).
pub fn kill(group: &Group) {
    kill_group(group.pgid);
}

/// SIGKILL every process in group `pgid` (no error when the group is already empty).
fn kill_group(pgid: i32) {
    if pgid > 1 {
        // SAFETY: killpg only sends a signal; a group id we created, never 0 or 1.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

fn tell_reaper(line: &str) {
    let Ok(mut guard) = REAPER.lock() else {
        return;
    };
    if let Some((_, stdin)) = guard.as_mut() {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }
}

extern "C" fn on_signal(signal: libc::c_int) {
    for slot in &LIVE {
        let pgid = slot.load(Ordering::SeqCst);
        if pgid > 1 {
            // SAFETY: killpg and _exit are async-signal-safe; atomics take no lock.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    // SAFETY: async-signal-safe exit; the reaper sees its pipe close and checks again.
    unsafe { libc::_exit(128 + signal) }
}

/// Kill every tracked group and exit on SIGINT, SIGTERM or SIGHUP.
pub fn install_signal_handlers() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: installs a handler that only calls async-signal-safe functions.
        unsafe {
            libc::signal(signal, on_signal as *const () as libc::sighandler_t);
        }
    }
}

/// Start the reaper: this executable as `psoxide-test reap`, in its own process group so a
/// terminal Ctrl-C reaches the harness (which cleans up) and not the reaper. Best effort: without
/// it the drop guards and signal handlers still run; only a SIGKILL of the harness would leak.
pub fn start_reaper() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command
        .arg("reap")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let Some(stdin) = child.stdin.take() else {
        return;
    };
    if let Ok(mut guard) = REAPER.lock() {
        *guard = Some((child, stdin));
    }
}

/// The reaper's body (`psoxide-test reap`): track `+pgid` / `-pgid` lines on stdin; when stdin
/// closes (the harness ended, however), SIGKILL every group still listed.
pub fn reap() -> ! {
    // SAFETY: ignoring terminal signals; the reaper must outlive an interrupted harness.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let mut live = std::collections::BTreeSet::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if let Some(n) = line.strip_prefix('+').and_then(|n| n.parse::<i32>().ok()) {
            live.insert(n);
        } else if let Some(n) = line.strip_prefix('-').and_then(|n| n.parse::<i32>().ok()) {
            live.remove(&n);
        }
    }
    for pgid in live {
        kill_group(pgid);
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_alive(pgid: i32) -> bool {
        // SAFETY: signal 0 only checks that the group has a member.
        unsafe { libc::killpg(pgid, 0) == 0 }
    }

    #[test]
    fn dropping_the_group_kills_grandchildren() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & sleep 30 & wait"])
            .stdout(Stdio::null());
        let (mut child, group) = spawn(&mut command).unwrap();
        let pgid = group.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(group_alive(pgid));
        drop(group);
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!group_alive(pgid), "group {pgid} still has members");
        assert!(LIVE.iter().all(|s| s.load(Ordering::SeqCst) != pgid));
    }

    #[test]
    fn a_grandchild_left_behind_by_its_parent_is_killed() {
        // The leader exits at once and leaves a background sleep: the group outlives it.
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 &"]).stdout(Stdio::null());
        let (mut child, group) = spawn(&mut command).unwrap();
        let pgid = group.id();
        let _ = child.wait();
        assert!(group_alive(pgid));
        drop(group);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!group_alive(pgid));
    }
}
