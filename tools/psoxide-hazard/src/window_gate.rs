//! `window-gate`: prove a PS-EXE's scratchpad stack window cannot overflow.
//!
//! `psx_rt::scratchpad_window` keeps the stack of whole call trees in the scratchpad and hops back to
//! the RAM stack at guarded callees. Its safety rests on numbers the game writes by hand (`need` per
//! guard) and on rules about the code behind the stubs, so this gate reads them back from the final
//! image:
//!
//! * every **guard** stub's room (`addiu $t8,$zero,room`) covers `floor + bound`, where `bound` is the
//!   static worst-case stack depth of its target (each function's frame plus 8, the deepest callee
//!   chain, other stubs counted as 24 bytes);
//! * every **level-1** stub's target bound fits the window (`top - floor`);
//! * nothing calls (`jal`) or tail-jumps (`j`) to a stubbed target except its own stub, which would
//!   skip the switch or the guard;
//! * no function in the guard-free closure makes an indirect call (`jalr`), adjusts `$sp` twice, or
//!   recurses without a guard.
//!
//! Stubs are found by their code: `lui $t9; addiu $t9; j __psx_rt_window_l1|__psx_rt_window_guard`.
//!
//! ```text
//! window-gate path/to/game.exe --map path/to/game.map [--floor N] [--top N] [--emit]
//! ```
//! `--emit` prints `EMIT <function> <room>` lines, the smallest guard room per target.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use crate::linkmap::{io_message, LinkMap};
use crate::listing::{load_address, word_at_offset, HEADER};

/// Usage text.
pub const USAGE: &str = "\
Fail when a scratchpad stack window can overflow.

    window-gate path/to/game.exe --map path/to/game.map [--floor N] [--top N] [--emit]

--floor is the first scratchpad byte the window may use (default 8), --top the window's stack top
(default 976, psx_rt::scratchpad_window::TOP). --emit prints the room each guard needs.
";

/// One stub found in the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stub {
    /// Function index of the stub.
    pub at: usize,
    /// Function index of its target.
    pub target: usize,
    /// A guard (true) or level-1 stub (false).
    pub guard: bool,
    /// Room the guard asks for, floor included.
    pub room: u32,
}

/// What the gate concluded.
#[derive(Debug, Default)]
pub struct Report {
    /// One line per check.
    pub lines: Vec<String>,
    /// False when any check failed.
    pub ok: bool,
    /// `(target name, smallest room)` for `--emit`.
    pub rooms: Vec<(String, u32)>,
}

/// A function of the image: address, size and name.
#[derive(Clone, Debug)]
pub struct Func {
    /// Start address.
    pub start: i64,
    /// Size in bytes.
    pub size: i64,
    /// Symbol name.
    pub name: String,
}

fn short(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Run the checks over `words` (one per 4 bytes from `base`) and the function list.
/// Functions that never run on the window in a way that matters: they test `is_on_scratchpad_stack`/`is_active` first and
/// call their closure in place (psx-rt's nested `ScratchpadStack::run`), so their trampoline is unreachable there.
pub const DEFAULT_ALLOWED: &[&str] = &["__psx_rt_call_on_stack"];

/// Run the checks; see the module docs.
pub fn analyse(words: &dyn Fn(i64) -> u32, funcs: &[Func], floor: u32, top: u32) -> Report {
    let n = funcs.len();
    let find = |a: i64| -> Option<usize> {
        let j = funcs.partition_point(|g| g.start <= a);
        (j > 0 && a < funcs[j - 1].start + funcs[j - 1].size.max(4)).then(|| j - 1)
    };
    let addr_of = |name: &str| funcs.iter().find(|f| f.name == name).map(|f| f.start);
    let l1 = addr_of("__psx_rt_window_l1");
    let guard_common = addr_of("__psx_rt_window_guard");
    let hop = addr_of("__psx_rt_window_hop");
    let mut stubs: Vec<Option<Stub>> = vec![None; n];
    for (i, f) in funcs.iter().enumerate() {
        if f.size < 16 {
            continue;
        }
        let w: Vec<u32> = (0..4).map(|k| words(f.start + 4 * k)).collect();
        if w[0] >> 16 != 0x3c19 || w[1] >> 16 != 0x2739 || w[2] >> 26 != 2 {
            continue;
        }
        let jt = i64::from((f.start as u32 & 0xf000_0000) | ((w[2] & 0x03ff_ffff) << 2));
        let is_guard = Some(jt) == guard_common;
        if !is_guard && Some(jt) != l1 {
            continue;
        }
        let target =
            i64::from(((w[0] & 0xffff) << 16).wrapping_add((w[1] & 0xffff) as i16 as i32 as u32));
        let room = if is_guard && w[3] >> 16 == 0x2418 {
            w[3] & 0xffff
        } else {
            0
        };
        if let Some(t) = find(target) {
            stubs[i] = Some(Stub {
                at: i,
                target: t,
                guard: is_guard,
                room,
            });
        }
    }
    let allowed: BTreeSet<usize> = DEFAULT_ALLOWED
        .iter()
        .filter_map(|nm| funcs.iter().position(|f| f.name == *nm))
        .collect();
    let common: BTreeSet<usize> = [l1, guard_common, hop]
        .iter()
        .flatten()
        .filter_map(|&a| find(a))
        .collect();
    let stubbed: BTreeSet<usize> = stubs.iter().flatten().map(|s| s.target).collect();
    let mut frame = vec![0u32; n];
    let mut multi = vec![0u32; n];
    let mut indirect = vec![0u32; n];
    let mut edges: Vec<Vec<usize>> = vec![vec![]; n];
    let mut bypass = Vec::new();
    for (i, f) in funcs.iter().enumerate() {
        if stubs[i].is_some() || common.contains(&i) || allowed.contains(&i) || f.size <= 0 {
            continue;
        }
        for k in 0..f.size / 4 {
            let w = words(f.start + 4 * k);
            if w >> 16 == 0x27bd && ((w & 0xffff) as i16) < 0 {
                if frame[i] == 0 {
                    frame[i] = (-((w & 0xffff) as i16 as i32)) as u32;
                } else {
                    multi[i] += 1;
                }
            }
            if w >> 26 == 3 || w >> 26 == 2 {
                let t = i64::from((f.start as u32 & 0xf000_0000) | ((w & 0x03ff_ffff) << 2));
                if let Some(j) = find(t) {
                    if j == i || allowed.contains(&j) {
                        continue;
                    }
                    if stubbed.contains(&j) && t == funcs[j].start && stubs[j].is_none() {
                        bypass.push(format!("{} -> {}", f.name, funcs[j].name));
                    }
                    edges[i].push(j);
                }
            } else if w >> 26 == 0 && w & 0x3f == 9 {
                indirect[i] += 1;
            }
        }
    }
    struct Ctx<'a> {
        frame: &'a [u32],
        edges: &'a [Vec<usize>],
        stubs: &'a [Option<Stub>],
        memo: Vec<Option<u32>>,
        onpath: Vec<bool>,
        cyc: Vec<usize>,
    }
    fn bound(c: &mut Ctx<'_>, i: usize) -> u32 {
        if let Some(d) = c.memo[i] {
            return d;
        }
        if c.onpath[i] {
            c.cyc.push(i);
            return 0;
        }
        c.onpath[i] = true;
        let mut best = 0;
        for k in 0..c.edges[i].len() {
            let e = c.edges[i][k];
            let d = match c.stubs[e] {
                Some(s) if s.guard => 24,
                Some(s) => bound(c, s.target),
                None => bound(c, e),
            };
            best = best.max(d);
        }
        c.onpath[i] = false;
        let d = c.frame[i] + 8 + best;
        c.memo[i] = Some(d);
        d
    }
    let mut ctx = Ctx {
        frame: &frame,
        edges: &edges,
        stubs: &stubs,
        memo: vec![None; n],
        onpath: vec![false; n],
        cyc: vec![],
    };
    let mut report = Report {
        ok: true,
        ..Report::default()
    };
    for s in stubs.iter().flatten() {
        ctx.cyc.clear();
        let b = bound(&mut ctx, s.target);
        let name = short(&funcs[s.target].name).to_string();
        let cyc = !ctx.cyc.is_empty();
        let smallest = (b + 7) / 8 * 8 + floor;
        if s.guard {
            let good = s.room >= floor + b && !cyc && indirect[s.target] == 0;
            report.lines.push(format!(
                "{} guard {name}: room {} needs {} (frame {}, bound {b}){}{}",
                if good { "PASS" } else { "FAIL" },
                s.room,
                floor + b,
                frame[s.target],
                if cyc { " unguarded recursion" } else { "" },
                if indirect[s.target] > 0 {
                    " indirect call"
                } else {
                    ""
                }
            ));
            report.ok &= good;
            report.rooms.push((name, smallest));
        } else {
            let good = b <= top.saturating_sub(floor) && !cyc;
            report.lines.push(format!(
                "{} level1 {name}: bound {b} window {}",
                if good { "PASS" } else { "FAIL" },
                top.saturating_sub(floor)
            ));
            report.ok &= good;
        }
    }
    let mut reach = vec![false; n];
    let mut work: Vec<usize> = stubs.iter().flatten().map(|s| s.target).collect();
    while let Some(x) = work.pop() {
        if std::mem::replace(&mut reach[x], true) {
            continue;
        }
        for &c in &edges[x] {
            if stubs[c].is_none() && !common.contains(&c) {
                work.push(c);
            }
        }
    }
    let mut bad = BTreeMap::new();
    for i in 0..n {
        if reach[i] && (indirect[i] > 0 || multi[i] > 0) {
            bad.insert(i, (indirect[i], multi[i]));
        }
    }
    for (i, (ind, mul)) in bad {
        report.lines.push(format!("FAIL {} in the guard-free closure: {ind} indirect call(s), {mul} extra $sp adjustment(s)", funcs[i].name));
        report.ok = false;
    }
    for b in bypass {
        report
            .lines
            .push(format!("FAIL direct call skips the stub: {b}"));
        report.ok = false;
    }
    report.lines.push(format!(
        "{} stubs, {} functions in the guard-free closure: {}",
        stubs.iter().flatten().count(),
        reach.iter().filter(|&&r| r).count(),
        if report.ok {
            "window proof ok"
        } else {
            "window proof FAILED"
        }
    ));
    report
}

/// Command line entry.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let floor_default = std::env::var("PSOXIDE_WINDOW_FLOOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8u32);
    let (mut exe, mut map_path, mut floor, mut top, mut emit) =
        (None, None, floor_default, 976u32, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--map" => map_path = it.next().cloned(),
            "--floor" => floor = it.next().and_then(|v| v.parse().ok()).unwrap_or(floor),
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
    let data = match std::fs::read(&exe) {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(out, "{}", io_message(&e, Path::new(&exe)));
            return 2;
        }
    };
    let map = match LinkMap::open(Path::new(&map_path)) {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 2;
        }
    };
    let base = load_address(&data);
    let words = |a: i64| word_at_offset(&data, a - base + HEADER).unwrap_or(0);
    let mut funcs: Vec<Func> = map
        .sorted_names()
        .into_iter()
        .map(|(a, names)| {
            let (size, name) = names.iter().max().cloned().unwrap_or((0, String::new()));
            Func {
                start: a,
                size,
                name,
            }
        })
        .collect();
    funcs.sort_by_key(|f| f.start);
    let r = analyse(&words, &funcs, floor, top);
    if r.lines.len() > 1 {
        for l in &r.lines {
            let _ = writeln!(out, "{l}");
        }
    }
    if emit {
        for (n, room) in &r.rooms {
            let _ = writeln!(out, "EMIT {n} {room}");
        }
    }
    i32::from(!r.ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUI_T9: u32 = 0x3c19_0000;
    const ADDIU_T9: u32 = 0x2739_0000;
    fn j(addr: u32) -> u32 {
        0x0800_0000 | ((addr >> 2) & 0x03ff_ffff)
    }
    fn jal(addr: u32) -> u32 {
        0x0c00_0000 | ((addr >> 2) & 0x03ff_ffff)
    }
    const ADDIU_SP: u32 = 0x27bd_0000;

    /// Words by address from a sparse list.
    fn image(parts: &[(i64, Vec<u32>)]) -> impl Fn(i64) -> u32 + '_ {
        move |a| {
            for (s, w) in parts {
                if a >= *s && a < *s + 4 * w.len() as i64 {
                    return w[((a - s) / 4) as usize];
                }
            }
            0
        }
    }

    fn func(start: i64, size: i64, name: &str) -> Func {
        Func {
            start,
            size,
            name: name.to_string(),
        }
    }

    /// level1 stub -> root(frame 64) -> guard stub -> callee(frame `callee_frame`).
    fn scene(room: u32, callee_frame: u32) -> (Vec<(i64, Vec<u32>)>, Vec<Func>) {
        let (l1, gd, hp) = (0x8001_0000i64, 0x8001_0100i64, 0x8001_0200i64);
        let (s_root, root, s_callee, callee) = (
            0x8001_0300i64,
            0x8001_0400i64,
            0x8001_0500i64,
            0x8001_0600i64,
        );
        let parts = vec![
            (l1, vec![0; 8]),
            (gd, vec![0; 8]),
            (hp, vec![0; 8]),
            (
                s_root,
                vec![LUI_T9 | 0x8001, ADDIU_T9 | 0x0400, j(l1 as u32), 0],
            ),
            (
                root,
                vec![ADDIU_SP | (0x10000 - 64), jal(s_callee as u32), 0, 0],
            ),
            (
                s_callee,
                vec![
                    LUI_T9 | 0x8001,
                    ADDIU_T9 | 0x0600,
                    j(gd as u32),
                    0x2418_0000 | room,
                ],
            ),
            (callee, vec![ADDIU_SP | (0x10000 - callee_frame), 0, 0, 0]),
        ];
        let funcs = vec![
            func(l1, 32, "__psx_rt_window_l1"),
            func(gd, 32, "__psx_rt_window_guard"),
            func(hp, 32, "__psx_rt_window_hop"),
            func(s_root, 16, "__ws_root"),
            func(root, 16, "root"),
            func(s_callee, 16, "__ws_callee"),
            func(callee, 16, "callee"),
        ];
        (parts, funcs)
    }

    #[test]
    fn a_guard_with_enough_room_passes() {
        let (parts, funcs) = scene(8 + 128 + 8, 120);
        let r = analyse(&image(&parts), &funcs, 8, 976);
        assert!(r.ok, "{:?}", r.lines);
    }

    #[test]
    fn a_guard_whose_room_is_below_its_bound_fails() {
        let (parts, funcs) = scene(8 + 64, 120);
        let r = analyse(&image(&parts), &funcs, 8, 976);
        assert!(!r.ok);
        assert!(r.lines.iter().any(|l| l.starts_with("FAIL guard callee")));
    }

    #[test]
    fn a_direct_call_that_skips_the_stub_fails() {
        let (mut parts, funcs) = scene(8 + 128 + 8, 120);
        parts[4].1[1] = jal(0x8001_0600);
        let r = analyse(&image(&parts), &funcs, 8, 976);
        assert!(!r.ok);
        assert!(r.lines.iter().any(|l| l.contains("skips the stub")));
    }

    #[test]
    fn an_indirect_call_in_the_closure_fails() {
        let (mut parts, funcs) = scene(8 + 128 + 8, 120);
        parts[4].1[2] = 0x0320_f809;
        let r = analyse(&image(&parts), &funcs, 8, 976);
        assert!(!r.ok);
    }

    #[test]
    fn a_root_deeper_than_the_window_fails() {
        let (parts, funcs) = scene(8 + 128 + 8, 120);
        let r = analyse(&image(&parts), &funcs, 8, 40);
        assert!(!r.ok);
    }
}
