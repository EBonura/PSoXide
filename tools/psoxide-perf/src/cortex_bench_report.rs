//! Summarise a `tools/cortex_bench.sh` output directory as one benchmark row.
//!
//! Reads run-1.txt / run-2.txt (frontend launch stdout), route-N.csv and
//! cycles-N.csv, checks the two replays agree byte for byte, writes
//! `summary.json`, and prints a before/after table when `--baseline <dir>` names
//! an earlier output directory. Frame rate is display-start flips per vblank over
//! the whole tape; the cycle attribution is the emulator's own accounting.
//!
//! Under the bench's default `lockstep-visuals` build every visual frame is
//! rendered, so the bus cycles to tape end scale with the work done and are the
//! primary number (they include RAM, I-cache and interlock stalls, which retired
//! instruction counts do not). `work_instructions` (retired instructions outside
//! `App::run_scheduled`, the vblank wait loop, symbolised against the guest link
//! map) is the secondary, stall-blind number. On a shipping-cadence build
//! (`CORTEX_BENCH_FEATURES="cd-stream-bench"`) the tape ends after a fixed number
//! of simulation ticks, bus cycles are constant by construction, and idle share
//! plus flips are what move.

use std::io::Write;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::pyjson::Json;
use crate::util::{
    dict_rows, parse_hex, parse_int, read_text, round_places, usage_error, Cli, Error, Result,
    Token,
};

/// The per-category cycle columns of a `--cpu-cycle-profile-log`.
pub const CYCLE_COLUMNS: [&str; 8] = [
    "issue_cycles",
    "ram_load_stall_cycles",
    "stack_ram_load_stall_cycles",
    "ram_store_stall_cycles",
    "mmio_stall_cycles",
    "icache_refill_stall_cycles",
    "gte_busy_stall_cycles",
    "muldiv_interlock_stall_cycles",
];

/// `(address, name)` symbols of a guest link map, sorted.
pub fn load_map(path: &Path) -> Result<(Vec<u64>, Vec<String>)> {
    let row = Regex::new(r"^\s*([0-9a-f]{8})\s+[0-9a-f]{8}\s+([0-9a-f]+)\s+\d+\s{9,}(\S.*)$")
        .expect("static pattern");
    let mut symbols: Vec<(u64, String)> = Vec::new();
    for line in crate::util::splitlines(&read_text(path)?) {
        let Some(captures) = row.captures(line) else {
            continue;
        };
        let name = &captures[3];
        if [".", "BYTE", "LONG", "SHORT", "QUAD", "FILL", "<internal"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        let address = u64::from_str_radix(&captures[1], 16).map_err(|e| Error(e.to_string()))?;
        symbols.push((address, name.trim().to_string()));
    }
    symbols.sort();
    Ok((
        symbols.iter().map(|s| s.0).collect(),
        symbols.into_iter().map(|s| s.1).collect(),
    ))
}

/// `(idle, work)` retired instructions from the exact PC-line log.
pub fn idle_split(out: &Path, run: u32) -> Result<(i64, i64)> {
    let (addrs, names) = load_map(&out.join("link.map"))?;
    let (mut idle, mut work) = (0i64, 0i64);
    let (_, rows) = dict_rows(&read_text(&out.join(format!("pcline-{run}.csv")))?)?;
    for row in &rows {
        let pc = parse_hex(row.get("line_pc").ok_or("KeyError: 'line_pc'")?)? as u64;
        let count = parse_int(row.get("instructions").ok_or("KeyError: 'instructions'")?)?;
        let right = addrs.partition_point(|&a| a <= pc);
        let name = if right > 0 {
            names[right - 1].as_str()
        } else {
            ""
        };
        if name.contains("run_scheduled") {
            idle += count;
        } else {
            work += count;
        }
    }
    Ok((idle, work))
}

/// Read actual guest counters/hashes from `launch` stdout without inferring a
/// frame rate. Keys are in the order the Python tool inserted them.
pub fn parse_launch_summary(text: &str) -> Result<Json> {
    let grab = |key: &str| -> Option<String> {
        let pattern = Regex::new(&format!(r"(?m)^{key}=(\S+)")).expect("static pattern");
        pattern.captures(text).map(|c| c[1].to_string())
    };
    let tick = Regex::new(r"(?m)^tick=(\d+)\s+cycles=(\d+)")
        .expect("static pattern")
        .captures(text)
        .ok_or("launch stdout has no tick/cycles completion line")?;
    let missing = |key: &str| {
        Error(format!(
            "'NoneType' object has no attribute 'group' ({key} line missing)"
        ))
    };
    let mut summary = Json::object([
        ("instructions", Json::Int(parse_int(&tick[1])?)),
        ("bus_cycles", Json::Int(parse_int(&tick[2])?)),
        (
            "route_ticks",
            Json::Int(parse_int(
                &grab("route-ticks").ok_or_else(|| missing("route-ticks"))?,
            )?),
        ),
        (
            "vram_hash",
            Json::Str(grab("vram_fnv1a_64").ok_or_else(|| missing("vram_fnv1a_64"))?),
        ),
        (
            "display_hash",
            Json::Str(grab("display_fnv1a_64").ok_or_else(|| missing("display_fnv1a_64"))?),
        ),
    ]);
    if let Some(stopped) = Regex::new(r"(?m)^tick=.*?stopped-at=(\d+)")
        .expect("static pattern")
        .captures(text)
    {
        summary.set("stopped_at", Json::Int(parse_int(&stopped[1])?));
    }
    if let Some(polls) = Regex::new(r"(?m)^route-ticks=(\d+)\s+port1-polls=(\d+)")
        .expect("static pattern")
        .captures(text)
    {
        summary.set("port1_polls", Json::Int(parse_int(&polls[2])?));
    }
    Ok(summary)
}

fn column_ints(rows: &[crate::util::DictRow], name: &str) -> Result<Vec<i64>> {
    rows.iter()
        .map(|row| {
            parse_int(
                row.get(name)
                    .ok_or_else(|| Error(format!("KeyError: '{name}'")))?,
            )
        })
        .collect()
}

/// The summary of replay `run` in `out`.
pub fn run_summary(out: &Path, run: u32) -> Result<Json> {
    let mut summary = parse_launch_summary(&read_text(&out.join(format!("run-{run}.txt")))?)?;
    let (_, route) = dict_rows(&read_text(&out.join(format!("route-{run}.csv")))?)?;
    let flips: i64 = column_ints(&route, "display_start_changed")?.iter().sum();
    let vblanks = route.len().saturating_sub(1).max(1) as f64;
    summary.set("flips", Json::Int(flips));
    summary.set(
        "fps",
        Json::Float(round_places(flips as f64 / (vblanks / 60.0), 3)),
    );
    if out.join(format!("pcline-{run}.csv")).exists() && out.join("link.map").exists() {
        let (idle, work) = idle_split(out, run)?;
        summary.set("idle_instructions", Json::Int(idle));
        summary.set("work_instructions", Json::Int(work));
        summary.set(
            "idle_percent",
            Json::Float(round_places(
                100.0 * idle as f64 / (idle + work).max(1) as f64,
                2,
            )),
        );
    }
    let (_, cycles) = dict_rows(&read_text(&out.join(format!("cycles-{run}.csv")))?)?;
    let profiled: i64 = column_ints(&cycles, "profiled_cpu_cycles")?.iter().sum();
    let total = if profiled == 0 { 1 } else { profiled } as f64;
    let mut shares = Json::Object(Vec::new());
    for column in CYCLE_COLUMNS {
        let sum: i64 = column_ints(&cycles, column)?.iter().sum();
        shares.set(
            column.replace("_cycles", ""),
            Json::Float(round_places(100.0 * sum as f64 / total, 2)),
        );
    }
    summary.set("cycle_shares", shares);
    Ok(summary)
}

fn text_of(value: &Json) -> String {
    match value {
        Json::Str(text) => text.clone(),
        Json::Int(value) => value.to_string(),
        Json::Float(value) => crate::util::float_repr(*value),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Null => "None".to_string(),
        other => other.dumps(None, false),
    }
}

/// Join like `pathlib`: the `.` components disappear.
fn join_display(base: &Path, name: &str) -> String {
    let mut path = PathBuf::new();
    for component in base.components() {
        if !matches!(component, std::path::Component::CurDir) {
            path.push(component);
        }
    }
    path.push(name);
    path.display().to_string()
}

const USAGE: &str = "psoxide-perf cortex-bench-report OUT_DIR [--baseline DIR]";

/// `psoxide-perf cortex-bench-report OUT_DIR [--baseline DIR]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let (mut out_dir, mut baseline): (Option<PathBuf>, Option<PathBuf>) = (None, None);
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        let step: std::result::Result<(), String> = match token {
            Token::Flag(flag) if flag == "--baseline" => {
                cli.value(&flag).map(|v| baseline = Some(v.into()))
            }
            Token::Flag(flag) => Err(format!("unrecognized arguments: {flag}")),
            Token::Positional(text) if out_dir.is_none() => {
                out_dir = Some(text.into());
                Ok(())
            }
            Token::Positional(text) => Err(format!("unrecognized arguments: {text}")),
        };
        if let Err(message) = step {
            return Ok(usage_error(USAGE, &message));
        }
    }
    let Some(out_dir) = out_dir else {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: out",
        ));
    };

    let mut one = run_summary(&out_dir, 1)?;
    let two = run_summary(&out_dir, 2)?;
    for key in ["vram_hash", "display_hash", "bus_cycles", "route_ticks"] {
        let (a, b) = (one.get(key), two.get(key));
        if !matches!((a, b), (Some(a), Some(b)) if a.py_eq(b)) {
            eprintln!(
                "cortex-bench: FAIL: replays disagree on {key}: {} vs {}",
                a.map_or("None".to_string(), text_of),
                b.map_or("None".to_string(), text_of)
            );
            return Ok(1);
        }
    }
    let gate_path = out_dir.join("symbol-gate.txt");
    let gate = if gate_path.exists() {
        read_text(&gate_path)?.trim().to_string()
    } else {
        String::new()
    };
    one.set(
        "symbol_gate",
        Json::from(if gate.starts_with("guest-symbol-gate: PASS") {
            "PASS"
        } else {
            "FAIL"
        }),
    );
    let exe_path = out_dir.join("exe.sha256");
    let exe_sha = if exe_path.exists() {
        read_text(&exe_path)?
            .split_whitespace()
            .next()
            .ok_or("list index out of range")?
            .to_string()
    } else {
        String::new()
    };
    one.set("exe_sha256", Json::Str(exe_sha));
    std::fs::write(out_dir.join("summary.json"), one.dumps(Some(2), false))?;

    let mut rows: Vec<(&str, Json)> = vec![("after", one.clone())];
    if let Some(baseline) = &baseline {
        let loaded = Json::parse(&read_text(&baseline.join("summary.json"))?)?;
        rows.insert(0, ("before", loaded));
    }
    let columns = [
        "bus_cycles",
        "work_instructions",
        "idle_percent",
        "flips",
        "vram_hash",
        "display_hash",
        "symbol_gate",
    ];
    for (_, row) in rows.iter_mut() {
        if row.get("work_instructions").is_none() {
            row.set("work_instructions", Json::from("n/a"));
        }
        if row.get("idle_percent").is_none() {
            row.set("idle_percent", Json::from("n/a"));
        }
    }
    writeln!(
        out,
        "| run | {} | ram_load% | icache% | muldiv% |",
        columns.join(" | ")
    )?;
    writeln!(out, "|---|{}", "---|".repeat(columns.len() + 3))?;
    for (name, row) in &rows {
        let share = |key: &str| -> Result<String> {
            Ok(text_of(
                row.get("cycle_shares")
                    .and_then(|s| s.get(key))
                    .ok_or_else(|| Error(format!("KeyError: '{key}'")))?,
            ))
        };
        let cells: Result<Vec<String>> = columns
            .iter()
            .map(|c| {
                row.get(c)
                    .map(text_of)
                    .ok_or_else(|| Error(format!("KeyError: '{c}'")))
            })
            .collect();
        writeln!(
            out,
            "| {name} | {} | {} | {} | {} |",
            cells?.join(" | "),
            share("ram_load_stall")?,
            share("icache_refill_stall")?,
            share("muldiv_interlock_stall")?
        )?;
    }
    if baseline.is_some() {
        let b = &rows[0].1;
        let same = one
            .get("vram_hash")
            .zip(b.get("vram_hash"))
            .is_some_and(|(x, y)| x.py_eq(y))
            && one
                .get("display_hash")
                .zip(b.get("display_hash"))
                .is_some_and(|(x, y)| x.py_eq(y));
        let int_of = |v: &Json, key: &str| -> Result<i64> {
            v.get(key)
                .and_then(Json::as_i64)
                .ok_or_else(|| Error(format!("KeyError: '{key}'")))
        };
        let cycles = 100.0 * (int_of(&one, "bus_cycles")? - int_of(b, "bus_cycles")?) as f64
            / int_of(b, "bus_cycles")? as f64;
        let mut line = format!(
            "\nbus cycles: {cycles:+.3}% (the number that matters under lockstep: stalls included)"
        );
        if let (Some(old), Some(new)) = (
            b.get("work_instructions").and_then(Json::as_i64),
            one.get("work_instructions").and_then(Json::as_i64),
        ) {
            let delta = 100.0 * (new - old) as f64 / old as f64;
            line.push_str(&format!("; work instructions: {delta:+.3}%"));
        }
        writeln!(
            out,
            "{line}  ({})",
            if same {
                "hashes identical"
            } else {
                "HASHES DIFFER: visual change, needs the 6.2 A/B"
            }
        )?;
    }
    writeln!(
        out,
        "\ncortex-bench: PASS (two replays identical; summary in {})",
        join_display(&out_dir, "summary.json")
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_summary_reads_counters_in_order() {
        let text = "tick=10  cycles=40  pc=0x80010000  stopped-at=10\nroute-ticks=2 port1-polls=5\nvram_fnv1a_64=0x1\ndisplay_fnv1a_64=0x2  w=320  h=240\n";
        let summary = parse_launch_summary(text).unwrap();
        assert_eq!(
            summary.dumps(None, false),
            r#"{"instructions": 10, "bus_cycles": 40, "route_ticks": 2, "vram_hash": "0x1", "display_hash": "0x2", "stopped_at": 10, "port1_polls": 5}"#
        );
        assert!(parse_launch_summary("nothing here").is_err());
    }

    #[test]
    fn map_rows_need_nine_spaces_before_the_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link.map");
        std::fs::write(
            &path,
            "80010000 80010000       54     1         _start\n80010054 80010054       10     1 ./x.o:(.text)\n8001ff00 8001ff00 4 1         .hidden\n",
        )
        .unwrap();
        let (addrs, names) = load_map(&path).unwrap();
        assert_eq!(addrs, vec![0x8001_0000]);
        assert_eq!(names, vec!["_start".to_string()]);
    }
}
