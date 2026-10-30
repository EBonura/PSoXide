//! `window-gate`: prove a PS-EXE's scratchpad stack window cannot overflow.
//!
//! `psx_rt::scratchpad_window` keeps the stack of whole call trees in the scratchpad and hops back to
//! the RAM stack at guarded callees. Its safety rests on numbers the game writes by hand and on rules about the
//! code behind the stubs, so this gate reads them back from the final image with `stack-guard`'s walker
//! (hazard trampolines, jump tables, `$sp` writes, recursion, the map check):
//!
//! * every stub carries the window floor (the word after its `j`); all stubs must agree and `--floor` /
//!   `PSOXIDE_WINDOW_FLOOR`, when given, must match;
//! * every **guard** stub's room (`addiu $t8,$zero,room`) covers `floor` + its target's static depth + 20, other
//!   guards counted as 24 bytes;
//! * every **level-1** stub's target fits the window (`top - floor`);
//! * nothing calls (`jal`) or tail-jumps (`j`) to a stubbed target except its own stub, which would skip the
//!   switch or the guard;
//! * the walker refuses indirect calls, `jr` that is not a proven table, `$sp` writes other than `addiu`
//!   frames (including realignment), recursion (a self `jal` included) and calls outside known code.
//!
//! Stubs are found by their code: `lui $t9; addiu $t9; j __psx_rt_window_l1|__psx_rt_window_guard`.
//!
//! ```text
//! window-gate path/to/game.exe --map path/to/game.map [--floor N] [--top N] [--emit]
//! ```
//! `--emit` prints `EMIT <function> <need>` lines, the smallest `NEED` for `window_fn!` per target.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use crate::linkmap::io_message;
use crate::stack_guard::{short, tree_depth, GuardImage};

/// Usage text.
pub const USAGE: &str = "\
Fail when a scratchpad stack window can overflow.

    window-gate path/to/game.exe --map path/to/game.map [--floor N] [--top N] [--emit]

--floor is the window floor the stubs must carry (default: from PSOXIDE_WINDOW_FLOOR, else the stubs'
own), --top the window's stack top (default 976, psx_rt::scratchpad_window::TOP). --emit prints the room
each guard needs (the `NEED` of `window_fn!`).
";

/// `STACK_OVERHEAD` of `stack-guard`: bytes a tree cannot use at the bottom of its region.
const OVERHEAD: i64 = 20;
/// What a guard stub costs on the window when it does not hop (a hop frame is 16 bytes).
const GUARD_COST: i64 = 24;

/// One stub found in the image.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stub {
    /// Its address.
    pub at: i64,
    /// Its target function's address.
    pub target: i64,
    /// A guard (true) or level-1 stub (false).
    pub guard: bool,
    /// Room the guard asks for, floor included.
    pub room: u32,
    /// The floor it carries.
    pub floor: u32,
}

/// What the gate concluded.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Report {
    /// One line per check.
    pub lines: Vec<String>,
    /// False when any check failed.
    pub ok: bool,
    /// Number of stubs found.
    pub stubs: usize,
    /// `(target name, smallest NEED)` for `--emit`: what `window_fn!`'s `guard FLOOR, NEED` takes.
    pub rooms: Vec<(String, u32)>,
}

fn find_stubs(image: &GuardImage) -> Vec<Stub> {
    let named = |name: &str| {
        image
            .map
            .names
            .iter()
            .find(|(_, v)| v.iter().any(|(_, n)| n == name))
            .map(|(a, _)| *a)
    };
    let (l1, guard) = (named("__psx_rt_window_l1"), named("__psx_rt_window_guard"));
    let mut stubs = Vec::new();
    for (at, names) in image.map.sorted_names() {
        if names.iter().map(|(s, _)| *s).max().unwrap_or(0) < 20 {
            continue;
        }
        let w = |k: i64| image.word_at(at + 4 * k) as u32;
        if w(0) >> 16 != 0x3c19 || w(1) >> 16 != 0x2739 || w(2) >> 26 != 2 {
            continue;
        }
        let jt = i64::from(((at as u32) & 0xf000_0000) | ((w(2) & 0x03ff_ffff) << 2));
        let is_guard = Some(jt) == guard;
        if !is_guard && Some(jt) != l1 {
            continue;
        }
        let target =
            i64::from(((w(0) & 0xffff) << 16).wrapping_add((w(1) & 0xffff) as i16 as i32 as u32));
        let room = if is_guard && w(3) >> 16 == 0x2418 {
            w(3) & 0xffff
        } else {
            0
        };
        // The floor rides in the fifth word as `addiu $zero,$zero,floor`.
        if w(4) >> 16 != 0x2400 {
            continue;
        }
        stubs.push(Stub {
            at,
            target,
            guard: is_guard,
            room,
            floor: w(4) & 0xffff,
        });
    }
    stubs
}

/// Run the checks over the image.
#[must_use]
pub fn analyse(image: &GuardImage, floor_arg: Option<u32>, top: u32) -> Report {
    let stubs = find_stubs(image);
    let mut report = Report {
        ok: true,
        stubs: stubs.len(),
        ..Report::default()
    };
    if stubs.is_empty() {
        return report;
    }
    let name_of = |a: i64| {
        image
            .map
            .function(a)
            .map_or_else(|| format!("{a:08x}"), |(_, _, n)| short(&n))
    };
    let floor = stubs[0].floor;
    let fail = |report: &mut Report, line: String| {
        report.lines.push(format!("FAIL {line}"));
        report.ok = false;
    };
    for s in stubs.iter().filter(|s| s.floor != floor) {
        fail(
            &mut report,
            format!(
                "stub {} carries floor {} but others carry {floor}",
                name_of(s.at),
                s.floor
            ),
        );
    }
    if let Some(f) = floor_arg {
        if f != floor {
            fail(
                &mut report,
                format!("stubs carry floor {floor} but {f} was asked for"),
            );
        }
    }
    let leaves: HashMap<i64, i64> = stubs
        .iter()
        .filter(|s| s.guard)
        .map(|s| (s.at, GUARD_COST))
        .collect();
    for s in &stubs {
        let name = name_of(s.target);
        match tree_depth(image, s.target, &leaves) {
            Err(e) => fail(
                &mut report,
                format!("{} {name}: {e}", if s.guard { "guard" } else { "level1" }),
            ),
            Ok((depth, _)) => {
                let needed = depth + OVERHEAD;
                let smallest = (i64::from(floor) + needed + 7) / 8 * 8;
                if s.guard {
                    let good = i64::from(s.room) >= i64::from(floor) + needed;
                    report.lines.push(format!(
                        "{} guard {name}: room {} needs {} (depth {depth})",
                        if good { "PASS" } else { "FAIL" },
                        s.room,
                        i64::from(floor) + needed
                    ));
                    report.ok &= good;
                    report
                        .rooms
                        .push((name, (smallest - i64::from(floor)) as u32));
                } else {
                    let window = i64::from(top) - i64::from(floor);
                    let good = needed <= window;
                    report.lines.push(format!(
                        "{} level1 {name}: depth {depth} window {window}",
                        if good { "PASS" } else { "FAIL" }
                    ));
                    report.ok &= good;
                }
            }
        }
    }
    // A stubbed target called directly skips its switch or guard.
    let targets: HashMap<i64, ()> = stubs.iter().map(|s| (s.target, ())).collect();
    let stub_ranges: Vec<(i64, i64)> = stubs.iter().map(|s| (s.at, s.at + 20)).collect();
    for (addr, entry) in image.listing.iter() {
        if entry.op != "jal" && entry.op != "j" {
            continue;
        }
        let Some(dest) = crate::text::trailing_hex(&entry.args) else {
            continue;
        };
        if targets.contains_key(&dest) && !stub_ranges.iter().any(|&(a, b)| a <= addr && addr < b) {
            if let Some((start, end, name)) = image.map.function(addr) {
                if name.starts_with("__psx_rt_window_")
                    || (start <= dest && dest < end && entry.op == "j")
                {
                    continue;
                }
                fail(
                    &mut report,
                    format!(
                        "direct {} skips the stub: {} -> {}",
                        entry.op,
                        short(&name),
                        name_of(dest)
                    ),
                );
            }
        }
    }
    let commons: Vec<i64> = [
        "__psx_rt_window_l1",
        "__psx_rt_window_guard",
        "__psx_rt_window_hop",
    ]
    .iter()
    .filter_map(|n| {
        image
            .map
            .names
            .iter()
            .find(|(_, v)| v.iter().any(|(_, x)| x == n))
            .map(|(a, _)| *a)
    })
    .collect();
    for (addr, entry) in image.listing.iter() {
        if entry.op != "j" && entry.op != "jal" {
            continue;
        }
        let Some(dest) = crate::text::trailing_hex(&entry.args) else {
            continue;
        };
        if commons.contains(&dest) && !stub_ranges.iter().any(|&(a, b)| a <= addr && addr < b) {
            let inside = image
                .map
                .function(addr)
                .is_some_and(|(_, _, n)| n.starts_with("__psx_rt_window_"));
            if !inside {
                fail(
                    &mut report,
                    format!("{addr:08x} jumps to a window routine but is not a recognised stub"),
                );
            }
        }
    }
    report.lines.push(format!(
        "{} stubs: {}",
        stubs.len(),
        if report.ok {
            "window proof ok"
        } else {
            "window proof FAILED"
        }
    ));
    report
}

/// Open `exe` and `map_path` and run the gate.
///
/// # Errors
///
/// The image or its map cannot be read, or the map is from another link.
pub fn check(exe: &Path, map_path: &Path, floor: Option<u32>, top: u32) -> Result<Report, String> {
    let image = GuardImage::open(exe, map_path).map_err(|e| e.to_string())?;
    Ok(analyse(&image, floor, top))
}

/// Command line entry.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let floor_env = std::env::var("PSOXIDE_WINDOW_FLOOR")
        .ok()
        .and_then(|v| v.parse().ok());
    let (mut exe, mut map_path, mut floor, mut top, mut emit) =
        (None, None, floor_env, 976u32, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--map" => map_path = it.next().cloned(),
            "--floor" => floor = it.next().and_then(|v| v.parse().ok()).or(floor),
            "--top" => top = it.next().and_then(|v| v.parse().ok()).unwrap_or(top),
            "--emit" => emit = true,
            f if f.starts_with("--") => {}
            _ => exe = Some(a.clone()),
        }
    }
    let (Some(exe), Some(map_path)) = (exe, map_path) else {
        let _ = write!(out, "{USAGE}");
        return 2;
    };
    if let Err(e) = std::fs::metadata(&exe) {
        let _ = writeln!(out, "{}", io_message(&e, Path::new(&exe)));
        return 2;
    }
    let report = match check(Path::new(&exe), Path::new(&map_path), floor, top) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 2;
        }
    };
    if report.stubs > 0 {
        for l in &report.lines {
            let _ = writeln!(out, "{l}");
        }
    }
    if emit {
        for (n, room) in &report.rooms {
            let _ = writeln!(out, "EMIT {n} {room}");
        }
    }
    i32::from(!report.ok)
}
