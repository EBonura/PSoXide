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
//!
//! Every layer kills a group only while it is still ours. Once a group's leader has been reaped
//! and its last member has exited, the kernel may hand the same number to a new process, which
//! may belong to another lane on the same machine. Each group therefore records its leader's
//! start time when it is tracked, and a kill is skipped when a process with the group's number
//! exists and started at a different time. A group whose leader is gone but whose members remain
//! is still ours: the kernel does not reuse a number while a group of that number exists.

use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Live process groups, readable from a signal handler (no locks there).
static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

/// The leader start time of the group in the same `LIVE` slot (0 = unknown).
static STARTS: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

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
    /// The leader's start time when the group was tracked (0 = unknown).
    start: u64,
}

impl Group {
    fn track(pgid: i32) -> Self {
        // The leader is our unreaped child here, so its number cannot have been reused yet.
        let start = start_time(pgid).unwrap_or(0);
        if pgid > 0 {
            if let Some(index) = LIVE.iter().position(|s| {
                s.compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            }) {
                STARTS[index].store(start, Ordering::SeqCst);
            }
            tell_reaper(&format!("+{pgid} {start}"));
        }
        Group { pgid, start }
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
        kill_group(self.pgid, self.start);
        for slot in &LIVE {
            let _ = slot.compare_exchange(self.pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
        tell_reaper(&format!("-{}", self.pgid));
    }
}

/// SIGKILL every process in `group` now (it stays tracked until dropped).
pub fn kill(group: &Group) {
    kill_group(group.pgid, group.start);
}

/// SIGKILL every process in group `pgid` whose leader started at `start` (no error when the group
/// is already empty; nothing is sent when the number now belongs to another process).
fn kill_group(pgid: i32, start: u64) {
    if pgid > 1 && still_ours(pgid, start) {
        // SAFETY: killpg only sends a signal; a group id we created, never 0 or 1.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// False only when a process numbered `pgid` exists and started at a time other than `start`:
/// the number was reused after our group ended. An unknown start (0) keeps the old behaviour.
fn still_ours(pgid: i32, start: u64) -> bool {
    match start_time(pgid) {
        Some(now) => start == 0 || now == start,
        // No such process: either our group lives on without its leader (the number cannot be
        // reused while it does) or the group is empty and killpg does nothing.
        None => true,
    }
}

/// Start time of process `pid` in microseconds since the epoch, or `None` when there is no such
/// process (or this platform cannot say). One system call, no allocation: usable from the
/// signal handler.
#[cfg(target_os = "macos")]
fn start_time(pid: i32) -> Option<u64> {
    // SAFETY: proc_bsdinfo is plain data, so all zeroes is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes, as PROC_PIDTBSDINFO expects.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size)
        .then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
        .filter(|&t| t != 0)
}

/// Start time of process `pid`: not read on this platform, so every kill goes ahead as before.
#[cfg(not(target_os = "macos"))]
fn start_time(_pid: i32) -> Option<u64> {
    None
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
    for (slot, start) in LIVE.iter().zip(&STARTS) {
        let pgid = slot.load(Ordering::SeqCst);
        // kill_group makes one proc_pidinfo call and one killpg; atomics take no lock.
        kill_group(pgid, start.load(Ordering::SeqCst));
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

/// The reaper's body (`psoxide-test reap`): track `+pgid start` / `-pgid` lines on stdin; when
/// stdin closes (the harness ended, however), SIGKILL every group still listed and still ours.
pub fn reap() -> ! {
    // SAFETY: ignoring terminal signals; the reaper must outlive an interrupted harness.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let mut live = std::collections::BTreeMap::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if let Some((pgid, start)) = line.strip_prefix('+').and_then(parse_tracked) {
            live.insert(pgid, start);
        } else if let Some(n) = line.strip_prefix('-').and_then(|n| n.parse::<i32>().ok()) {
            live.remove(&n);
        }
    }
    for (pgid, start) in live {
        kill_group(pgid, start);
    }
    std::process::exit(0)
}

/// `pgid start` (start may be absent: unknown, 0) from a reaper `+` line.
fn parse_tracked(fields: &str) -> Option<(i32, u64)> {
    let mut fields = fields.split_whitespace();
    let pgid = fields.next()?.parse().ok()?;
    let start = match fields.next() {
        Some(start) => start.parse().ok()?,
        None => 0,
    };
    Some((pgid, start))
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

    #[cfg(target_os = "macos")]
    #[test]
    fn a_group_number_reused_by_another_process_is_left_alone() {
        // Another lane's group that happens to have the number our ended group had.
        let mut command = Command::new("sleep");
        command.arg("30").stdout(Stdio::null());
        let (mut child, theirs) = spawn(&mut command).unwrap();
        let pgid = theirs.id();
        let their_start = start_time(pgid).expect("a live child has a start time");
        // Our stale record of that number: a leader that started at another time.
        let ours = Group {
            pgid,
            start: their_start - 1,
        };
        kill(&ours);
        drop(ours);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(group_alive(pgid), "a reused group number was killed");
        drop(theirs);
        let _ = child.wait();
        assert!(!group_alive(pgid));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_live_process_has_a_start_time() {
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(start_time(pid).is_some());
        assert!(still_ours(pid, start_time(pid).unwrap()));
        assert!(
            still_ours(pid, 0),
            "an unknown start keeps the old behaviour"
        );
    }

    #[test]
    fn reaper_lines_parse_with_and_without_a_start() {
        assert_eq!(parse_tracked("123 456"), Some((123, 456)));
        assert_eq!(parse_tracked("123"), Some((123, 0)));
        assert_eq!(parse_tracked("x"), None);
        assert_eq!(parse_tracked("123 y"), None);
    }
}
